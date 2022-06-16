#![crate_type = "proc-macro"]

use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};

// TODO consolidate this / collapse other consts into this crate
const SENSEMAKER_ZOME_NAME: &str = "sensemaker_main";

// TODO think about hdk_extern and which zome/happ it goes into. will the widgets want
// to invoke a macro, similar to `sensemaker_cell_id_fns`, s.t. the hdk_extern registers
// in their wasm?
#[proc_macro_attribute]
pub fn expand_remote_calls(_attrs: TokenStream, item: TokenStream) -> TokenStream {
    // expand_remote_calls is only valid for functions
    let item_fn = syn::parse_macro_input!(item as syn::ItemFn);
    let fn_name = item_fn.sig.ident.to_string();

    let mut new_fn = item_fn.clone();

    // prefix fn ident.
    new_fn.sig.ident = Ident::new(&format!("remote_{}", fn_name), Span::call_site());

    // arg list. tuple munging.
    {
        let arg_pat_type = match item_fn
            .sig
            .inputs
            .first()
            .expect("hdk fn should have 1 arg")
        {
            syn::FnArg::Typed(pat_type) => pat_type,
            _ => panic!("expand_remote_calls: invalid Receiver FnArg"),
        };
        let arg_pat_type_ty = &arg_pat_type.ty;
        let token_stream = (quote::quote! {
            (cell_id, cap_secret, payload): (CellId, Option<CapSecret>, #arg_pat_type_ty)
        })
        .into();
        let tup_arg = syn::parse_macro_input!(token_stream as syn::FnArg);
        assert!(new_fn.sig.inputs.pop().is_some());
        assert!(new_fn.sig.inputs.is_empty());
        new_fn.sig.inputs.push(tup_arg);
    }

    // body with bridge call.
    {
        let token_stream = (quote::quote! {
            {
                match call(
                    CallTargetCell::Other(cell_id),
                    #SENSEMAKER_ZOME_NAME.into(),
                    #fn_name.into(),
                    cap_secret,
                    payload,
                )? {
                    ZomeCallResponse::Ok(response) => Ok(response.decode()?),
                    err => {
                        error!("ZomeCallResponse error: {:?}", err);
                        Err(WasmError::Guest(format!("{}: {:?}", #fn_name, err)))
                    }
                }
            }
        })
        .into();
        let fn_body = syn::parse_macro_input!(token_stream as syn::Block);
        new_fn.block = Box::new(fn_body);
    }

    let doc_comment = format!("make a bridge call to `{}`", fn_name);
    (quote::quote! {
        #[hdk_extern]
        #item_fn

        #[doc = #doc_comment]
        #[hdk_extern]
        #new_fn
    })
    .into()
}
