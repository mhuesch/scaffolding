use combine::{stream::position, EasyParser, StreamOnce};
use futures::executor;
use holochain_conductor_client::{AppWebsocket, ZomeCall};
use holochain_types::{dna::DnaBundle, prelude::CellId};
use holochain_zome_types::zome_io::ExternIO;
use holo_hash::{HeaderHash, HoloHash};
use scrawl;
use std::{error, io, iter, path::Path};
use termion::{event::Key, input::MouseTerminal, raw::IntoRawMode, screen::AlternateScreen};
use tui::{
    backend::TermionBackend,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Span, Spans, Text},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};

use common::CreateInterchangeEntryInput;
use rep_lang_concrete_syntax::parse::expr;
use rep_lang_core::abstract_syntax::Expr;
use rep_lang_runtime::{env::Env, infer::infer_expr, types::Scheme};

#[allow(dead_code)]
mod event;
use event::{Event, Events};

#[derive(Debug, Clone)]
pub enum ExprState {
    Valid(Scheme, Expr),
    Invalid(String),
}

impl ExprState {
    fn is_valid(&self) -> bool {
        match self {
            ExprState::Valid(_, _) => true,
            ExprState::Invalid(_) => false,
        }
    }
}

struct App {
    expr_input: String,
    expr_state: ExprState,
    opt_events: Option<Events>,
    #[allow(dead_code)]
    hc_ws: AppWebsocket,
    hc_response: String,
}

impl App {
    fn new(app_url: String) -> App {
        // TODO make this async to avoid TUI hangs, and also to allow graceful
        // dis/connection without requiring TUI restarts.
        let hc_ws = executor::block_on(AppWebsocket::connect(app_url)).expect("connect failed");
        App {
            expr_input: String::new(),
            expr_state: ExprState::Invalid("init".into()),
            opt_events: Some(Events::new()),
            hc_ws,
            hc_response: "".into(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn error::Error>> {
    // terminal initialization
    let stdout = io::stdout().into_raw_mode()?;
    let stdout = MouseTerminal::from(stdout);
    let stdout = AlternateScreen::from(stdout);
    let backend = TermionBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new("ws://127.0.0.1:9999".into());

    loop {
        // draw UI
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints(
                    [
                        Constraint::Length(1),
                        Constraint::Length(25),
                        Constraint::Min(1),
                        Constraint::Length(4),
                    ]
                    .as_ref(),
                )
                .split(f.size());

            let mut default_commands = vec![
                Span::raw("press "),
                Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" to exit, "),
                Span::styled("e", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" to launch $EDITOR"),
            ];
            let mut valid_expr_commands = vec![
                Span::raw(", "),
                Span::styled("c", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" to create entry"),
            ];
            let msg = {
                if app.expr_state.is_valid() {
                    default_commands.append(&mut valid_expr_commands);
                }
                default_commands.push(Span::raw("."));
                default_commands
            };

            let style = Style::default().add_modifier(Modifier::RAPID_BLINK);
            let mut text = Text::from(Spans::from(msg));
            text.patch_style(style);
            let help_message = Paragraph::new(text);
            f.render_widget(help_message, chunks[0]);

            let expr_input = Paragraph::new(app.expr_input.as_ref())
                .style(Style::default())
                .block(Block::default().borders(Borders::ALL).title("expr input"));
            f.render_widget(expr_input, chunks[1]);

            let msgs = Paragraph::new(format!("{:?}", app.expr_state))
                .style(Style::default())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("feedback on expr"),
                );
            f.render_widget(msgs, chunks[2]);

            let app_info = Paragraph::new(format!("{}", app.hc_response))
                .style(Style::default())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("holochain response"),
                );
            f.render_widget(app_info, chunks[3]);
        })?;

        // handle input
        let Event::Input(input) = {
            match app.opt_events {
                None => panic!("impossible: logic error"),
                Some(ref itr) => itr.next()?,
            }
        };
        match input {
            Key::Char('q') => {
                terminal.clear().expect("clear failed");
                break;
            }
            Key::Char('e') => {
                app.opt_events = None;
                terminal.clear().expect("clear failed");
                app.expr_input = scrawl::with(&app.expr_input)?;
                app.opt_events = Some(Events::new());
                terminal.clear().expect("clear failed");
                let st = match expr().easy_parse(position::Stream::new(&app.expr_input[..])) {
                    Err(err) => ExprState::Invalid(format!("parse error:\n\n{}\n", err)),
                    Ok((expr, extra_input)) => {
                        if extra_input.is_partial() {
                            ExprState::Invalid(format!(
                                "error: unconsumed input: {:?}",
                                extra_input
                            ))
                        } else {
                            match infer_expr(&Env::new(), &expr) {
                                Ok(sc) => ExprState::Valid(sc, expr),
                                Err(err) => ExprState::Invalid(format!("type error: {:?}", err)),
                            }
                        }
                    }
                };
                app.expr_state = st;
            }
            Key::Char('c') => {
                match &app.expr_state {
                    ExprState::Invalid(_) => {}
                    ExprState::Valid(_sc, expr) => {
                        let input: CreateInterchangeEntryInput = CreateInterchangeEntryInput {
                            expr: expr.clone(),
                            args: Vec::new(),
                        };
                        let payload = ExternIO::encode(input).unwrap();
                        let agent_pk_bytes: Vec<u8> = iter::repeat(1).take(36).collect();
                        let agent_pk = HoloHash::from_raw_36(agent_pk_bytes);
                        // TODO do all of this async, with jobs spawned at TUI
                        // start time, and store results in `App`
                        let cell_id = {
                            let path = Path::new("./happs/rep_interchange/rep_interchange.dna");
                            let bundle = DnaBundle::read_from_file(path)
                                .await
                                .unwrap();
                            let (_dna_file, dna_hash) = bundle.into_dna_file(None, None)
                                .await
                                .unwrap();
                            CellId::new(dna_hash, agent_pk.clone())
                        };
                        let zc = ZomeCall {
                            cell_id,
                            zome_name: "interpreter".into(),
                            fn_name: "create_interchange_entry".into(),
                            payload,
                            cap: None,
                            provenance: agent_pk,
                        };
                        // TODO \/ we have a problem here: CellMissing
                        let result = app.hc_ws.zome_call(zc).await.unwrap();
                        let ie_hash: HeaderHash = result.decode().unwrap();
                        app.hc_response = format!("create: ie_hash: {:?}", ie_hash);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
