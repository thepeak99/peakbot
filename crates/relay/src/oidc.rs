//! Hand-rolled OIDC for a confidential client: discovery, authorize URL, code
//! exchange, JWKS key selection and ID-token verification (design §5.3).
//!
//! Discovery and JWKS are fetched fresh per login (no cache, design §2);
//! `jsonwebtoken` 11 (aws_lc_rs) does the JWS + registered claims.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64U;
use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use sha2::{Digest, Sha256};

use crate::config::Issuer;
use crate::limits::HTTP_TIMEOUT;

/// The IdP boundary (design §5.3).
#[non_exhaustive]
pub struct Oidc {
    issuers: Vec<Issuer>,
    http: reqwest::Client,
    /// `{public_url}/oidc/callback` — derived, never configured (design §5.3).
    redirect_uri: String,
    /// Loopback `http` endpoints are allowed only for loopback deployments
    /// (tests), mirroring the `public_url` rule (design §5.3).
    loopback_ok: bool,
}

impl Oidc {
    pub fn new(issuers: Vec<Issuer>, origin: &str) -> Result<Self, OidcError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|_| OidcError::Other("http client".into()))?;
        let loopback_ok = origin.starts_with("http://")
            && matches!(
                url::Url::parse(origin)
                    .ok()
                    .as_ref()
                    .and_then(|u| u.host_str()),
                Some("127.0.0.1") | Some("::1") | Some("localhost")
            );
        Ok(Self {
            issuers,
            http,
            redirect_uri: format!("{origin}/oidc/callback"),
            loopback_ok,
        })
    }

    /// Issuer names, in order (for the `/login` chooser, design §4.1).
    pub fn issuer_names(&self) -> Vec<String> {
        self.issuers.iter().map(|i| i.name.clone()).collect()
    }

    /// Discover the issuer (fresh, no cache) and build the authorize URL with
    /// `scope=openid`, a random state, a random nonce and relay↔IdP PKCE S256.
    pub async fn authorize(&self, issuer: usize) -> Result<(url::Url, Flow), OidcError> {
        let iss = &self.issuers[issuer];
        let doc = self.discover(&iss.url).await?;
        let mut au = url::Url::parse(&doc.authorization_endpoint)
            .map_err(|_| OidcError::Discovery("authorization_endpoint".into()))?;
        let state = rand_b64u()?;
        let nonce = rand_b64u()?;
        let verifier = rand_b64u()?;
        let challenge = b64u(&sha256(verifier.as_bytes()));
        au.query_pairs_mut()
            .clear()
            .append_pair("response_type", "code")
            .append_pair("client_id", &iss.client_id.0)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("scope", "openid")
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok((
            au,
            Flow {
                issuer,
                state,
                nonce,
                verifier,
            },
        ))
    }

    /// Re-run discovery, exchange the code (client secret + PKCE verifier) and
    /// verify the ID token against the §5.3 table.
    pub async fn finish(&self, flow: &Flow, code: &str) -> Result<Identity, OidcError> {
        let iss = &self.issuers[flow.issuer];
        let doc = self.discover(&iss.url).await?;
        // `client_secret_basic` by default; post only when discovery lists
        // `client_secret_post` exclusively (design §5.3).
        let post_auth = matches!(
            doc.token_endpoint_auth_methods_supported.as_deref(),
            Some([m]) if m == "client_secret_post"
        );
        let mut form: Vec<(&str, String)> = vec![
            ("grant_type", "authorization_code".into()),
            ("code", code.to_string()),
            ("redirect_uri", self.redirect_uri.clone()),
            ("code_verifier", flow.verifier.clone()),
        ];
        let mut req = self.http.post(&doc.token_endpoint);
        if post_auth {
            form.push(("client_id", iss.client_id.0.clone()));
            form.push(("client_secret", iss.client_secret.as_str().to_string()));
        } else {
            req = req.basic_auth(&iss.client_id.0, Some(iss.client_secret.as_str()));
        }
        let resp = req
            .form(&form)
            .send()
            .await
            .map_err(|_| OidcError::Transport("token exchange".into()))?;
        if !resp.status().is_success() {
            return Err(OidcError::Transport("token exchange".into()));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|_| OidcError::Transport("token response".into()))?;
        let token_type = body
            .get("token_type")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if !token_type.eq_ignore_ascii_case("bearer") {
            return Err(OidcError::Other("token_type".into()));
        }
        // `access_token`/`refresh_token` are discarded unread (design §5.3).
        let id_token = body
            .get("id_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| OidcError::Other("id_token".into()))?
            .to_string();
        let jwks: JwkSet = self
            .http
            .get(&doc.jwks_uri)
            .send()
            .await
            .map_err(|_| OidcError::Transport("jwks".into()))?
            .json()
            .await
            .map_err(|_| OidcError::Transport("jwks".into()))?;
        self.verify(&id_token, &jwks, iss, &flow.nonce)
    }

    async fn discover(&self, issuer: &str) -> Result<Discovery, OidcError> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let doc: Discovery = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|_| OidcError::Transport("discovery".into()))?
            .json()
            .await
            .map_err(|_| OidcError::Discovery("response".into()))?;
        // Byte-for-byte issuer equality (design §5.3).
        if doc.issuer != issuer {
            return Err(OidcError::Discovery("issuer".into()));
        }
        for (name, endpoint) in [
            ("authorization_endpoint", &doc.authorization_endpoint),
            ("token_endpoint", &doc.token_endpoint),
            ("jwks_uri", &doc.jwks_uri),
        ] {
            self.check_endpoint(name, endpoint)?;
        }
        Ok(doc)
    }

    /// Endpoints must be https; loopback http only for loopback deployments.
    fn check_endpoint(&self, name: &str, endpoint: &str) -> Result<(), OidcError> {
        let u = url::Url::parse(endpoint).map_err(|_| OidcError::Discovery(name.into()))?;
        let loopback = matches!(
            u.host_str(),
            Some("127.0.0.1") | Some("::1") | Some("localhost")
        );
        match u.scheme() {
            "https" => Ok(()),
            "http" if loopback && self.loopback_ok => Ok(()),
            _ => Err(OidcError::Discovery(name.into())),
        }
    }

    /// ID-token validation, exactly the §5.3 table.
    fn verify(
        &self,
        token: &str,
        jwks: &JwkSet,
        iss: &Issuer,
        nonce: &str,
    ) -> Result<Identity, OidcError> {
        let id = |kind: &str| OidcError::IdToken(kind.to_string());
        // Alg allowlist first; the single allowed alg pins the decode.
        let header = decode_header(token).map_err(|_| id("header"))?;
        let (alg, kty) = match header.alg {
            Algorithm::RS256 => (Algorithm::RS256, "RSA"),
            Algorithm::ES256 => (Algorithm::ES256, "EC"),
            _ => return Err(id("alg")),
        };
        let jwk = select_key(jwks, header.kid.as_deref(), kty, &id)?;
        let mut validation = Validation::new(alg);
        validation.leeway = 60;
        validation.set_issuer(&[iss.url.as_str()]);
        validation.set_audience(&[iss.client_id.0.as_str()]);
        validation.validate_nbf = true;
        let key = DecodingKey::from_jwk(jwk).map_err(|_| id("jwk"))?;
        let data =
            decode::<serde_json::Value>(token, &key, &validation).map_err(|_| id("signature"))?;
        let claims = &data.claims;
        let now = crate::now_secs() as i64;
        // `aud`: string or array containing client_id (checked above); the
        // multi-value and `azp` rules are manual (design §5.3).
        let aud = match claims.get("aud") {
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            Some(serde_json::Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>(),
            _ => return Err(id("aud")),
        };
        if let Some(azp) = claims.get("azp").and_then(|v| v.as_str()) {
            if azp != iss.client_id.0 {
                return Err(id("azp"));
            }
        } else if aud.len() > 1 {
            return Err(id("azp"));
        }
        // `iat`: required, not future beyond skew, not older than the flow life.
        let iat = claims
            .get("iat")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| id("iat"))?;
        if iat > now + 60 || iat < now - 600 {
            return Err(id("iat"));
        }
        // `nonce`: required, constant-time equal to the flow's (design §5.3).
        let got = claims
            .get("nonce")
            .and_then(|v| v.as_str())
            .ok_or_else(|| id("nonce"))?;
        if !crate::ct_eq(got.as_bytes(), nonce.as_bytes()) {
            return Err(id("nonce"));
        }
        // `sub`: required, non-empty, ≤ 255 bytes.
        let subject = claims
            .get("sub")
            .and_then(|v| v.as_str())
            .ok_or_else(|| id("sub"))?;
        if subject.is_empty() || subject.len() > 255 {
            return Err(id("sub"));
        }
        Ok(Identity {
            issuer: iss.url.clone(),
            subject: subject.to_string(),
        })
    }
}

/// JWKS key selection: `kid` match, else exactly one key of the matching kty
/// (design §5.3). `use`/`alg` on the JWK must agree if present.
fn select_key<'a>(
    jwks: &'a JwkSet,
    kid: Option<&str>,
    kty: &str,
    id: &dyn Fn(&str) -> OidcError,
) -> Result<&'a Jwk, OidcError> {
    let candidates = jwks
        .keys
        .iter()
        .filter(|j| kid.map_or(true, |k| j.common.key_id.as_deref() == Some(k)))
        .filter(|j| jwk_matches(j, kty))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(id("kid"));
    }
    if kid.is_none() && candidates.len() > 1 {
        return Err(id("kid"));
    }
    Ok(candidates[0])
}

fn jwk_matches(jwk: &Jwk, kty: &str) -> bool {
    use jsonwebtoken::jwk::{KeyAlgorithm, PublicKeyUse};
    if let Some(use_) = &jwk.common.public_key_use {
        if !matches!(use_, PublicKeyUse::Signature) {
            return false;
        }
    }
    if let Some(alg) = &jwk.common.key_algorithm {
        let want = match kty {
            "RSA" => KeyAlgorithm::RS256,
            _ => KeyAlgorithm::ES256,
        };
        if alg != &want {
            return false;
        }
    }
    match (&jwk.algorithm, kty) {
        (jsonwebtoken::jwk::AlgorithmParameters::RSA(_), "RSA") => true,
        (jsonwebtoken::jwk::AlgorithmParameters::EllipticCurve(_), "EC") => true,
        _ => false,
    }
}

/// Discovery document (design §5.3); only the fields the relay consumes.
#[derive(serde::Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

/// One in-flight relay↔IdP flow half (design §3.3); the device half lives in
/// `http::Logins`. Removed at the callback, so single use.
#[derive(Debug, Clone)]
pub struct Flow {
    /// Index into `Oidc::issuers`.
    pub issuer: usize,
    /// Relay→IdP `state` (32 B b64url).
    pub state: String,
    /// The `nonce` sent to the IdP (32 B b64url); the ID token must carry it.
    pub nonce: String,
    /// Relay↔IdP PKCE verifier (RFC 7636).
    pub verifier: String,
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

fn b64u(bytes: &[u8]) -> String {
    B64U.encode(bytes)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(Sha256::digest(bytes).as_slice());
    out
}

/// 32 fresh OsRng bytes as b64url (states, nonces, verifiers).
fn rand_b64u() -> Result<String, OidcError> {
    let b = crate::random_32().map_err(|_| OidcError::Other("rng".into()))?;
    Ok(b64u(&b))
}
