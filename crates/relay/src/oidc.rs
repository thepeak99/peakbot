//! Hand-rolled OIDC for a confidential client: discovery, authorize URL, code
//! exchange, JWKS key selection and ID-token verification (design §5.3).
//!
//! SKELETON: signatures are the locked interface; bodies are `todo!()`.

/// The IdP boundary (design §5.3).
pub struct Oidc {
    _priv: (),
}

impl Oidc {
    /// Discover the issuer (fresh, no cache) and build the authorize URL with
    /// `scope=openid`, a random state, a random nonce and relay↔IdP PKCE S256.
    pub async fn authorize(&self, issuer: usize) -> Result<(url::Url, Flow), OidcError> {
        todo!("P2 task 5: discovery + authorize URL")
    }

    /// Re-run discovery, exchange the code (client secret + PKCE verifier) and
    /// verify the ID token against the §5.3 table.
    pub async fn finish(&self, flow: Flow, code: String) -> Result<Identity, OidcError> {
        todo!("P2 task 5: exchange + JWKS + ID-token validation")
    }
}

/// One in-flight `/login` flow (design §3.3). Stored in `Logins` under the
/// relay→IdP `state`; removed at the callback (single use).
pub struct Flow {
    _priv: (),
}

/// Verified identity: the account key is `(issuer, sub)` — never email/name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub issuer: String,
    pub subject: String,
}

/// OIDC failure. `IdToken` carries the kind only — never the token (design §4.9).
#[derive(Debug)]
pub enum OidcError {
    Discovery(String),
    /// ID-token validation failure; the kind is logged, the token is not.
    IdToken(String),
    Transport(String),
    Other(String),
}
