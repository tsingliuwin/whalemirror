//! dsh-lsp — the LSP capability seam and stdio backend for rustdsh.
//!
//! Mirrors the upstream trio:
//! - [`packages/lsp/lsp`](https://github.com/deepseek-ai/deepseek-harness/tree/main/packages/lsp/lsp)
//!   — the normalized seam vocabulary and provider registry (this module's `types` and
//!   [`LspRegistry`]).
//! - [`packages/lsp/lsp-stdio`](https://github.com/deepseek-ai/deepseek-harness/tree/main/packages/lsp/lsp-stdio)
//!   — the generic stdio backend ([`framing`], `client`, `provider`).
//! - [`packages/lsp/tool-lsp`](https://github.com/deepseek-ai/deepseek-harness/tree/main/packages/lsp/tool-lsp)
//!   — the model-facing `lsp` tool (`render`, `tool`).
//!
//! The seam exposes exactly the four semantic operations and no JSON-RPC escape hatch: a
//! provider reserves a branded id and an exclusive set of file extensions atomically, and
//! selection routes a query by the file's final extension — never by registration order.

pub mod cancel;
pub mod client;
pub mod framing;
pub mod host;
pub mod instance;
pub mod provider;
pub mod render;
pub mod translate;
pub mod types;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};

pub use cancel::{abort_error, CancelToken};
pub use client::{ConnectionSpec, LspConnection, ServerRequestHandler};
pub use host::{canonicalize_workspace, read_source, HostSource, HostWorkspace};
pub use instance::{InstanceSpec, LspInstance};
pub use provider::{StdioProvider, StdioProviderConfig};
pub use types::{
    LspHover, LspLocation, LspOperation, LspPosition, LspProvider, LspProviderQuery,
    LspQueryRequest, LspQueryResult, LspRange,
};

/// Stable LSP failure codes.
///
/// Upstream extends `HarnessError` with a stable `code` that callers route on instead of
/// parsing `message`; the same discipline applies here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LspErrorCode {
    /// A provider registration was structurally invalid.
    InvalidProvider,
    /// A provider id or extension was already reserved.
    Conflict,
    /// No provider handles the queried file.
    Unavailable,
    /// The service was disposed.
    Disposed,
    /// The operation is outside the closed union.
    UnsupportedOperation,
    /// A server response did not match the protocol.
    MalformedResponse,
    /// The server process failed or exited.
    ServerFailed,
    /// The query exceeded its time budget.
    Timeout,
    /// The document could not be read or opened.
    DocumentUnavailable,
}

impl LspErrorCode {
    /// The stable wire code, matching upstream's `code` strings.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidProvider => "LSP_INVALID_PROVIDER",
            Self::Conflict => "LSP_CONFLICT",
            Self::Unavailable => "LSP_UNAVAILABLE",
            Self::Disposed => "LSP_DISPOSED",
            Self::UnsupportedOperation => "LSP_UNSUPPORTED_OPERATION",
            Self::MalformedResponse => "LSP_MALFORMED_RESPONSE",
            Self::ServerFailed => "LSP_SERVER_FAILED",
            Self::Timeout => "LSP_TIMEOUT",
            Self::DocumentUnavailable => "LSP_DOCUMENT_UNAVAILABLE",
        }
    }
}

/// Structured LSP failure: a stable [`LspErrorCode`] plus a human message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspError {
    pub code: LspErrorCode,
    pub message: String,
}

impl LspError {
    pub fn new(code: LspErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    /// The stable wire code.
    pub fn code(&self) -> &'static str {
        self.code.as_str()
    }
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for LspError {}

/// Extract a file's final extension as a normalized, lowercase, leading-dot key
/// (e.g. `Foo.TS` → `.ts`, `foo.d.ts` → `.ts`).
///
/// Returns `""` for a name with no extension or a leading-dot dotfile (`.bashrc`), which
/// no route ever matches. Splits on both `/` and `\` so a caller's path separator does not
/// change the result.
pub fn final_extension(file_path: &str) -> String {
    let last_slash = file_path.rfind(['/', '\\']);
    let base = match last_slash {
        Some(index) => &file_path[index + 1..],
        None => file_path,
    };
    let Some(dot) = base.rfind('.') else { return String::new() };
    // dot == 0 is a leading-dot dotfile: no extension.
    if dot == 0 {
        return String::new();
    }
    base[dot..].to_lowercase()
}

/// Whether a normalized extension is well-formed: a dot followed by one or more non-dot,
/// non-separator chars.
fn is_valid_extension(ext: &str) -> bool {
    let mut chars = ext.chars();
    if chars.next() != Some('.') {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    !rest.is_empty() && rest.iter().all(|c| *c != '.' && *c != '/' && *c != '\\')
}

/// Lowercase an extension and ensure it carries a leading dot.
fn normalize_extension(ext: &str) -> String {
    let lower = ext.to_lowercase();
    if lower.starts_with('.') { lower } else { format!(".{lower}") }
}

/// One selection route: the provider to run plus the language id to synchronize the
/// document with.
#[derive(Clone)]
struct Route {
    provider: Arc<dyn LspProvider>,
    language_id: String,
}

#[derive(Default)]
struct RegistryState {
    provider_ids: HashSet<String>,
    routes: HashMap<String, Route>,
}

/// The LSP capability seam: provider registration/selection and normalized query
/// execution.
///
/// Holds the id reservations and the extension→route table; both are populated and
/// cleared together per provider so a route always has a live provider.
#[derive(Clone, Default)]
pub struct LspRegistry {
    state: Arc<Mutex<RegistryState>>,
}

impl LspRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider, atomically reserving its id and every normalized extension.
    ///
    /// Any conflict or invalid input publishes nothing and returns [`LspError`]. The
    /// returned [`LspRegistration`] releases every reservation when dropped or disposed.
    pub fn register_provider(
        &self,
        provider: Arc<dyn LspProvider>,
    ) -> Result<LspRegistration, LspError> {
        // Validate and conflict-check everything BEFORE any mutation: an invalid or
        // conflicting registration must publish nothing (fail-loud, all-or-nothing).
        let id = provider.id().to_string();
        if id.trim().is_empty() {
            return Err(LspError::new(
                LspErrorCode::InvalidProvider,
                "an LSP provider id must be a non-empty string",
            ));
        }
        let extensions = provider.extension_to_language();
        {
            let state = self.state.lock().unwrap();
            if state.provider_ids.contains(&id) {
                return Err(LspError::new(
                    LspErrorCode::Conflict,
                    format!("an LSP provider with id \"{id}\" is already registered"),
                ));
            }
            if extensions.is_empty() {
                return Err(LspError::new(
                    LspErrorCode::InvalidProvider,
                    format!("LSP provider \"{id}\" registers no file extensions"),
                ));
            }
        }

        // Normalize into this provider's route set, catching intra-provider duplicates
        // (e.g. `.TS` and `.ts`) before checking cross-provider conflicts.
        let mut pending: Vec<(String, String)> = Vec::new();
        for (raw_ext, language_id) in extensions {
            let ext = normalize_extension(raw_ext);
            if !is_valid_extension(&ext) {
                return Err(LspError::new(
                    LspErrorCode::InvalidProvider,
                    format!("LSP provider \"{id}\" maps an invalid extension \"{raw_ext}\""),
                ));
            }
            if language_id.trim().is_empty() {
                return Err(LspError::new(
                    LspErrorCode::InvalidProvider,
                    format!(
                        "LSP provider \"{id}\" maps extension \"{ext}\" to an empty language id"
                    ),
                ));
            }
            if pending.iter().any(|(known, _)| *known == ext) {
                return Err(LspError::new(
                    LspErrorCode::InvalidProvider,
                    format!("LSP provider \"{id}\" maps extension \"{ext}\" more than once"),
                ));
            }
            pending.push((ext, language_id.clone()));
        }

        {
            let mut state = self.state.lock().unwrap();
            for (ext, _) in &pending {
                if state.routes.contains_key(ext) {
                    return Err(LspError::new(
                        LspErrorCode::Conflict,
                        format!(
                            "extension \"{ext}\" is already handled by another LSP provider"
                        ),
                    ));
                }
            }
            // All checks passed: reserve id and every extension at once.
            state.provider_ids.insert(id.clone());
            for (ext, language_id) in &pending {
                state
                    .routes
                    .insert(ext.clone(), Route { provider: Arc::clone(&provider), language_id: language_id.clone() });
            }
        }

        Ok(LspRegistration {
            state: Arc::clone(&self.state),
            id,
            extensions: pending.into_iter().map(|(ext, _)| ext).collect(),
            active: true,
        })
    }

    /// Select a provider by the file's extension and run one query.
    ///
    /// Selection is per-query and order-independent; no match fails with
    /// `LSP_UNAVAILABLE`.
    pub async fn query(&self, request: LspQueryRequest) -> Result<LspQueryResult, LspError> {
        let extension = final_extension(&request.file_path);
        let route = {
            let state = self.state.lock().unwrap();
            state.routes.get(&extension).cloned()
        };
        let Some(route) = route else {
            return Err(LspError::new(
                LspErrorCode::Unavailable,
                format!("no LSP provider handles \"{}\"", request.file_path),
            ));
        };
        route
            .provider
            .query(LspProviderQuery { request, language_id: route.language_id })
            .await
    }

    /// The extensions currently routed, for diagnostics and tests.
    pub fn routed_extensions(&self) -> Vec<String> {
        let mut extensions: Vec<String> = self.state.lock().unwrap().routes.keys().cloned().collect();
        extensions.sort();
        extensions
    }
}

/// A live provider registration; dropping it releases the id and every extension.
pub struct LspRegistration {
    state: Arc<Mutex<RegistryState>>,
    id: String,
    extensions: Vec<String>,
    active: bool,
}

impl fmt::Debug for LspRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LspRegistration")
            .field("id", &self.id)
            .field("extensions", &self.extensions)
            .field("active", &self.active)
            .finish()
    }
}

impl LspRegistration {
    /// Release every reservation now (idempotent).
    pub fn dispose(mut self) {
        self.release();
    }

    fn release(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let mut state = self.state.lock().unwrap();
        state.provider_ids.remove(&self.id);
        for ext in &self.extensions {
            state.routes.remove(ext);
        }
    }
}

impl Drop for LspRegistration {
    fn drop(&mut self) {
        self.release();
    }
}