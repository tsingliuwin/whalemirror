//! LSP seam vocabulary: the normalized request, provider, and result contracts.
//!
//! Mirrors [`packages/lsp/lsp/src/types.ts`]. Positions and ranges are zero-based
//! UTF-16, matching the protocol; the model-facing tool owns the one-based cursor
//! convention. The seam exposes no protocol types, process or document controls, or
//! generic JSON-RPC escape hatch — only the four semantic operations.

use async_trait::async_trait;
use std::collections::HashMap;

use crate::LspError;

/// The four semantic queries the seam and model expose.
///
/// A closed union: adding an operation is a compile-enforced change across the seam,
/// providers, and the tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LspOperation {
    GoToDefinition,
    FindReferences,
    GoToImplementation,
    Hover,
}

impl LspOperation {
    /// The wire/operation name, matching the model-facing `enum`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GoToDefinition => "goToDefinition",
            Self::FindReferences => "findReferences",
            Self::GoToImplementation => "goToImplementation",
            Self::Hover => "hover",
        }
    }

    /// Parse an operation name; `None` for anything outside the closed set.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "goToDefinition" => Some(Self::GoToDefinition),
            "findReferences" => Some(Self::FindReferences),
            "goToImplementation" => Some(Self::GoToImplementation),
            "hover" => Some(Self::Hover),
            _ => None,
        }
    }

    /// The protocol method this operation issues.
    pub fn method(&self) -> &'static str {
        match self {
            Self::GoToDefinition => "textDocument/definition",
            Self::FindReferences => "textDocument/references",
            Self::GoToImplementation => "textDocument/implementation",
            Self::Hover => "textDocument/hover",
        }
    }

    /// Whether this operation normalizes to the `locations` result variant.
    pub fn is_navigation(&self) -> bool {
        !matches!(self, Self::Hover)
    }
}

/// A zero-based UTF-16 cursor coordinate, matching the LSP wire convention.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LspPosition {
    /// Zero-based line.
    pub line: u32,
    /// Zero-based UTF-16 code-unit offset within the line.
    pub character: u32,
}

/// A zero-based UTF-16 half-open range `[start, end)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

/// A caller's normalized query.
///
/// Every field is required: `workspace_root` is caller-supplied, `language_id` comes from
/// the provider registration (not here), and consumers own timeouts and result limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspQueryRequest {
    /// Which semantic query to run.
    pub operation: LspOperation,
    /// The source file to query (relative to `workspace_root` or absolute).
    pub file_path: String,
    /// The zero-based UTF-16 cursor position to query at.
    pub position: LspPosition,
    /// The workspace root the provider resolves against and indexes.
    pub workspace_root: String,
}

/// A request as a provider receives it: the caller's request plus the `language_id` the
/// seam derived from the provider's extension mapping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspProviderQuery {
    pub request: LspQueryRequest,
    /// The LSP language id for `file_path`, from this provider's extension mapping.
    pub language_id: String,
}

impl LspProviderQuery {
    pub fn operation(&self) -> LspOperation {
        self.request.operation
    }

    pub fn file_path(&self) -> &str {
        &self.request.file_path
    }

    pub fn position(&self) -> LspPosition {
        self.request.position
    }

    pub fn workspace_root(&self) -> &str {
        &self.request.workspace_root
    }
}

/// One resolved location: a document URI and the range within it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspLocation {
    /// The target document URI (`file:` or otherwise), verbatim from the server.
    pub uri: String,
    /// The range within the target document.
    pub range: LspRange,
}

/// Normalized hover content, or `None` for no hover at the position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspHover {
    /// The normalized hover text (markdown or plaintext, provider-joined).
    pub contents: String,
    /// The range the hover applies to, when the server supplied one.
    pub range: Option<LspRange>,
}

/// The closed result union.
///
/// Navigation operations normalize to `Locations`; `hover` normalizes to content or
/// `None`. The `Locations` variant carries `resolved_workspace_uri`: the provider's
/// canonical `file:` URI for the request's workspace root. A caller that relativizes
/// location URIs MUST use this, not parse the request's possibly symlinked process path
/// with host-platform rules; the execution platform may differ from the caller's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LspQueryResult {
    Locations {
        locations: Vec<LspLocation>,
        resolved_workspace_uri: String,
    },
    Hover {
        hover: Option<LspHover>,
    },
}

/// A language-server backend registered on the seam.
///
/// Each provider owns a stable id and an extension-to-language-id map (lowercase,
/// leading-dot keys). `findReferences` always includes declarations — the provider
/// enforces this internally; callers get no flag.
#[async_trait]
pub trait LspProvider: Send + Sync {
    /// Stable provider identity, reserved atomically with the extension mappings.
    fn id(&self) -> &str;

    /// Lowercase leading-dot extension → LSP language id (e.g. `{ ".ts": "typescript" }`).
    fn extension_to_language(&self) -> &HashMap<String, String>;

    /// Run one query. The seam has already selected this provider and derived
    /// `language_id`.
    async fn query(&self, request: LspProviderQuery) -> Result<LspQueryResult, LspError>;
}