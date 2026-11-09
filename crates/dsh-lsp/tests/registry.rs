//! Seam behaviour: extension derivation, atomic registration, and order-independent
//! routing.

use async_trait::async_trait;
use dsh_lsp::{
    final_extension, LspError, LspErrorCode, LspOperation, LspPosition, LspProvider,
    LspProviderQuery, LspQueryRequest, LspQueryResult, LspRegistry,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

struct FakeProvider {
    id: String,
    extensions: HashMap<String, String>,
    seen: Arc<Mutex<Vec<LspProviderQuery>>>,
}

impl FakeProvider {
    fn new(id: &str, extensions: &[(&str, &str)]) -> Arc<Self> {
        Arc::new(Self {
            id: id.to_string(),
            extensions: extensions
                .iter()
                .map(|(ext, language)| (ext.to_string(), language.to_string()))
                .collect(),
            seen: Arc::new(Mutex::new(Vec::new())),
        })
    }
}

#[async_trait]
impl LspProvider for FakeProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn extension_to_language(&self) -> &HashMap<String, String> {
        &self.extensions
    }

    async fn query(&self, request: LspProviderQuery) -> Result<LspQueryResult, LspError> {
        let workspace = request.workspace_root().to_string();
        self.seen.lock().unwrap().push(request);
        Ok(LspQueryResult::Locations { locations: Vec::new(), resolved_workspace_uri: workspace })
    }
}

fn request(file_path: &str, operation: LspOperation) -> LspQueryRequest {
    LspQueryRequest {
        operation,
        file_path: file_path.to_string(),
        position: LspPosition { line: 0, character: 0 },
        workspace_root: "/ws".to_string(),
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn derives_the_final_extension() {
    assert_eq!(final_extension("src/a.ts"), ".ts");
    assert_eq!(final_extension("Foo.TS"), ".ts");
    assert_eq!(final_extension("types/foo.d.ts"), ".ts");
    assert_eq!(final_extension(r"src\a.rs"), ".rs");
    assert_eq!(final_extension(".bashrc"), "");
    assert_eq!(final_extension("Makefile"), "");
    assert_eq!(final_extension("dir.with.dots/file"), "");
}

#[test]
fn rejects_invalid_registrations_without_publishing() {
    let registry = LspRegistry::new();

    let empty_id = FakeProvider::new("  ", &[(".ts", "typescript")]);
    let error = registry.register_provider(empty_id).unwrap_err();
    assert_eq!(error.code, LspErrorCode::InvalidProvider);
    assert_eq!(error.message, "an LSP provider id must be a non-empty string");

    let no_extensions = FakeProvider::new("ts", &[]);
    let error = registry.register_provider(no_extensions).unwrap_err();
    assert_eq!(error.message, "LSP provider \"ts\" registers no file extensions");

    let bad_extension = FakeProvider::new("ts", &[(".t/s", "typescript")]);
    let error = registry.register_provider(bad_extension).unwrap_err();
    assert_eq!(error.message, "LSP provider \"ts\" maps an invalid extension \".t/s\"");

    let empty_language = FakeProvider::new("ts", &[(".ts", " ")]);
    let error = registry.register_provider(empty_language).unwrap_err();
    assert_eq!(
        error.message,
        "LSP provider \"ts\" maps extension \".ts\" to an empty language id"
    );

    // `.TS` and `.ts` normalize to the same key: rejected intra-provider.
    let duplicate = FakeProvider::new("ts", &[(".ts", "typescript"), (".TS", "typescriptreact")]);
    let error = registry.register_provider(duplicate).unwrap_err();
    assert_eq!(
        error.message,
        "LSP provider \"ts\" maps extension \".ts\" more than once"
    );

    // Nothing published by any of the failures above.
    assert!(registry.routed_extensions().is_empty());
}

#[test]
fn rejects_conflicts_and_one_of_a_multi_extension_batch() {
    let registry = LspRegistry::new();
    let first = registry
        .register_provider(FakeProvider::new("rust", &[(".rs", "rust")]))
        .unwrap();

    let duplicate_id = FakeProvider::new("rust", &[(".toml", "toml")]);
    let error = registry.register_provider(duplicate_id).unwrap_err();
    assert_eq!(error.code, LspErrorCode::Conflict);
    assert_eq!(error.message, "an LSP provider with id \"rust\" is already registered");

    let duplicate_extension =
        FakeProvider::new("other", &[(".md", "markdown"), (".rs", "rust")]);
    let error = registry.register_provider(duplicate_extension).unwrap_err();
    assert_eq!(error.code, LspErrorCode::Conflict);
    assert_eq!(
        error.message,
        "extension \".rs\" is already handled by another LSP provider"
    );
    // The rejected batch published nothing, not even its free extension.
    assert_eq!(registry.routed_extensions(), vec![".rs".to_string()]);

    first.dispose();
    assert!(registry.routed_extensions().is_empty());
    // A disposed id is reusable.
    registry
        .register_provider(FakeProvider::new("rust", &[(".rs", "rust")]))
        .unwrap();
}

#[test]
fn routes_by_extension_and_passes_the_language_id() {
    let registry = LspRegistry::new();
    let provider = FakeProvider::new("ts", &[(".ts", "typescript"), (".tsx", "typescriptreact")]);
    let seen = Arc::clone(&provider.seen);
    let _registration = registry.register_provider(provider).unwrap();

    let result = block_on(registry.query(request("src/app.tsx", LspOperation::Hover))).unwrap();
    assert_eq!(
        result,
        LspQueryResult::Locations {
            locations: Vec::new(),
            resolved_workspace_uri: "/ws".to_string(),
        }
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].language_id, "typescriptreact");
    assert_eq!(seen[0].operation(), LspOperation::Hover);
}

#[test]
fn unknown_extensions_fail_unavailable() {
    let registry = LspRegistry::new();
    let _registration = registry
        .register_provider(FakeProvider::new("ts", &[(".ts", "typescript")]))
        .unwrap();
    let error = block_on(registry.query(request("src/app.py", LspOperation::GoToDefinition)))
        .unwrap_err();
    assert_eq!(error.code, LspErrorCode::Unavailable);
    assert_eq!(error.message, "no LSP provider handles \"src/app.py\"");
    assert_eq!(error.code(), "LSP_UNAVAILABLE");
}

#[test]
fn selection_is_order_independent() {
    let registry = LspRegistry::new();
    let _rust = registry
        .register_provider(FakeProvider::new("a", &[(".rs", "rust")]))
        .unwrap();
    let _ts = registry
        .register_provider(FakeProvider::new("b", &[(".ts", "typescript")]))
        .unwrap();
    assert_eq!(
        block_on(registry.query(request("x.ts", LspOperation::FindReferences))).is_ok(),
        true
    );
    assert_eq!(
        block_on(registry.query(request("x.rs", LspOperation::FindReferences))).is_ok(),
        true
    );
}