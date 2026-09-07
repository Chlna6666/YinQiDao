use proc_macro::{TokenStream, TokenTree};
use std::path::PathBuf;

fn compile_error(message: &str) -> TokenStream {
    format!("compile_error!({message:?});")
        .parse()
        .expect("compile_error token stream must parse")
}

#[proc_macro]
pub fn __icon_asset(input: TokenStream) -> TokenStream {
    let mut tokens = input.into_iter();
    let Some(token) = tokens.next() else {
        return compile_error("lucide_gpui::icon! expects one icon identifier");
    };
    if tokens.next().is_some() {
        return compile_error("lucide_gpui::icon! accepts exactly one icon identifier");
    }

    let TokenTree::Ident(identifier) = token else {
        return compile_error("lucide_gpui::icon! expects an identifier such as icon!(play)");
    };
    let stem = identifier.to_string().replace('_', "-");
    if stem.is_empty()
        || !stem
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return compile_error("Lucide icon identifiers must contain only lowercase letters, digits, and underscores");
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Some(crates_dir) = manifest_dir.parent() else {
        return compile_error("unable to locate the lucide-gpui icon directory");
    };
    let icon_path = crates_dir
        .join("lucide-gpui")
        .join("icons")
        .join(format!("{stem}.svg"));
    if !icon_path.is_file() {
        return compile_error(&format!("unknown Lucide icon `{stem}`"));
    }

    let asset_path = format!("lucide/{stem}.svg");
    let filesystem_path = icon_path.to_string_lossy();
    format!(
        "({asset_path:?}, include_bytes!({filesystem_path:?}) as &'static [u8])"
    )
    .parse()
    .expect("generated Lucide icon asset expression must parse")
}
