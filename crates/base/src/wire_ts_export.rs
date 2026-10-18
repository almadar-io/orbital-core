use crate::client_effect::ClientEffect;
use crate::client_effect::NavigateOptions;
use crate::oauth_ask::OAuthAsk;
use crate::oauth_ask::OAuthConnectionRow;
use crate::oauth_ask::OAuthProvider;
use crate::oauth_ask::OAuthReply;
use crate::oauth_ask::OAuthTokenGrant;
use crate::oauth_ask::PendingTake;
use crate::oauth_wire::Pending;
use crate::row_set::RowCompareOp;
use crate::row_set::RowPredicate;
use crate::dispatch_mode::DispatchMode;
use crate::theme::ResolvedTheme;
use crate::value::Value;
use std::path::PathBuf;
use ts_rs::Config;
use ts_rs::TypeVisitor;
use ts_rs::TS;


/// Declares a type whose TypeScript form is written here: `$name` is its exported name and
/// `$decl` its right-hand side.
macro_rules! declared_by_hand {
    ($ty:ty, $name:literal, $decl:literal $(, [$($dep:ty),*])?) => {
        impl TS for $ty {
            type WithoutGenerics = Self;
            type OptionInnerType = Self;
            fn name(_: &Config) -> String {
                $name.to_string()
            }
            fn inline(_: &Config) -> String {
                $name.to_string()
            }
            fn decl(_: &Config) -> String {
                format!("type {} = {}", $name, $decl)
            }
            fn decl_concrete(cfg: &Config) -> String {
                <Self as TS>::decl(cfg)
            }
            fn output_path() -> Option<PathBuf> {
                Some(PathBuf::from(concat!($name, ".ts")))
            }
            fn visit_dependencies(_v: &mut impl TypeVisitor)
            where
                Self: 'static,
            {
                $($(_v.visit::<$dep>();)*)?
            }
        }
    };
}


// The evaluator value serializes as plain JSON; `@almadar/core` calls that `JsonValue`.
impl TS for Value {
    type WithoutGenerics = Self;
    type OptionInnerType = Self;
    fn name(_: &Config) -> String {
        "JsonValue".to_string()
    }
    fn inline(_: &Config) -> String {
        "JsonValue".to_string()
    }
}

declared_by_hand!(
    ClientEffect,
    "ClientEffect",
    "[\"render-ui\", string, PatternConfig | null] | [\"render-ui\", string, PatternConfig | null, ResolvedPatternProps | null] | [\"render-ui\", string, PatternConfig | null, ResolvedPatternProps | null, number | null] | [\"navigate\", string] | [\"navigate\", string, { [key in string]: string } | null] | [\"navigate\", string, { [key in string]: string } | null, NavigateOptions] | [\"navigate-back\"]",
    [NavigateOptions]
);
declared_by_hand!(
    DispatchMode,
    "DispatchMode",
    "\"persistedAwaited\" | \"hybridClientOnly\" | \"runtimeOptimistic\""
);
declared_by_hand!(
    ResolvedTheme,
    "ResolvedTheme",
    "{ key: string, base: string, mode: \"light\" | \"dark\", tokens: JsonObject }"
);
declared_by_hand!(
    RowPredicate,
    "RowPredicate",
    "{ kind: \"compare\", field: string, op: RowCompareOp, value: FieldValue } | { kind: \"member\", field: string, values: Array<FieldValue> } | { kind: \"hasElement\", field: string, value: FieldValue } | { kind: \"contains\", field: string, text: string, caseSensitive: boolean } | { kind: \"isNull\", field: string, isNull: boolean } | { kind: \"all\", of: Array<RowPredicate> } | { kind: \"any\", of: Array<RowPredicate> } | { kind: \"not\", of: RowPredicate } | { kind: \"const\", value: boolean }",
    [RowCompareOp]
);
declared_by_hand!(OAuthProvider, "OAuthProvider", "\"google\" | \"github\" | \"auth0\"");
declared_by_hand!(
    Pending,
    "OAuthPendingGrant",
    "{ provider: string, redirectUri: string, verifier: string, subject: string | null, scopes: Array<string> }"
);
declared_by_hand!(
    OAuthTokenGrant,
    "OAuthTokenGrant",
    "{ accessToken: string, refreshToken: string | null, expiresIn: number, tokenType: string, scope: Array<string> | null, subject: string | null }"
);
declared_by_hand!(
    OAuthConnectionRow,
    "OAuthConnectionRow",
    "{ provider: OAuthProvider, connected: boolean, account: string, avatarUrl: string, scope: Array<string>, connectedAt: string }",
    [OAuthProvider]
);
declared_by_hand!(
    PendingTake,
    "OAuthPendingTake",
    "{ found: \"missing\" } | { found: \"expired\" } | { found: \"grant\", grant: OAuthPendingGrant }",
    [Pending]
);
declared_by_hand!(
    OAuthAsk,
    "OAuthAsk",
    "{ ask: \"pendingPut\", state: string, grant: OAuthPendingGrant, ttlMs: number } | { ask: \"pendingTake\", state: string } | { ask: \"connectionAvailable\" } | { ask: \"connectionSave\", provider: OAuthProvider, grant: OAuthTokenGrant } | { ask: \"connectionStatus\", provider: OAuthProvider } | { ask: \"connectionRemove\", provider: OAuthProvider } | { ask: \"subjectRemember\", accessToken: string, subject: string } | { ask: \"subjectLookup\", accessToken: string } | { ask: \"subjectForget\", accessToken: string } | { ask: \"metadataGet\", issuer: string } | { ask: \"metadataPut\", issuer: string, metadata: { [key in string]: JsonValue } } | { ask: \"clock\" } | { ask: \"mockTokenPut\", accessToken: string, refreshToken: string, expiresAtMs: number, provider: OAuthProvider } | { ask: \"mockTokenRefresh\", refreshToken: string, newAccessToken: string, expiresAtMs: number } | { ask: \"mockTokenRevoke\", token: string } | { ask: \"mockTokenCheck\", accessToken: string } | { ask: \"consent\", authUrl: string, state: string, redirectUri: string }",
    [Pending, OAuthTokenGrant, OAuthProvider]
);
declared_by_hand!(
    OAuthReply,
    "OAuthReply",
    "{ reply: \"done\" } | { reply: \"flag\", value: boolean } | { reply: \"taken\", taken: OAuthPendingTake } | { reply: \"row\", row: OAuthConnectionRow } | { reply: \"subject\", subject: string | null } | { reply: \"metadata\", metadata: { [key in string]: JsonValue } | null } | { reply: \"millis\", value: number } | { reply: \"mockToken\", state: \"missing\" | \"expired\" | \"valid\", provider: OAuthProvider | null } | { reply: \"code\", code: string | null }",
    [PendingTake, OAuthConnectionRow]
);
