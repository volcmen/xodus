use reqwest::Client;
use xal::{
    AuthPromptCallback, Constants, DeviceType, Flows, TokenStore, XalAppParameters,
    XalAuthenticator,
    client_params::CLIENT_WINDOWS,
    oauth2::{
        EmptyExtraTokenFields, RedirectUrl, Scope, StandardTokenResponse, basic::BasicTokenType,
    },
    response::{
        XADDisplayClaims, XATDisplayClaims, XAUDisplayClaims, XSTSDisplayClaims, XTokenResponse,
    },
};

use crate::{
    models::{live::ExchangeUserTokenOutcome, secrets::Token, soap},
    tokens::TokenManager,
};

fn get_app_params() -> XalAppParameters {
    XalAppParameters {
        client_id: "000000004424da1f".to_string(),
        title_id: Some("704208617".into()),
        auth_scopes: vec![Scope::new(
            xal::Constants::SCOPE_SERVICE_USER_AUTH.to_owned(),
        )],
        redirect_uri: Some(
            RedirectUrl::new(xal::Constants::OAUTH20_DESKTOP_REDIRECT_URL.into()).unwrap(),
        ),
        client_secret: None,
    }
}

pub async fn start_new_session(
    cb: impl AuthPromptCallback,
) -> Result<TokenStore, Box<dyn std::error::Error>> {
    let app_params = get_app_params();
    let mut authenticator = XalAuthenticator::new(app_params, CLIENT_WINDOWS(), "RETAIL".into());
    let ts = Flows::ms_authorization_flow(&mut authenticator, cb, true).await?;
    let ts = Flows::xbox_live_authorization_traditional_flow(
        &mut authenticator,
        ts.live_token,
        Constants::RELYING_PARTY_XBOXLIVE.to_string(),
        xal::AccessTokenPrefix::None,
        false,
    )
    .await?;
    Ok(ts)
}

pub async fn get_xsts_token(
    device_token: Option<&XTokenResponse<XADDisplayClaims>>,
    title_token: Option<&XTokenResponse<XATDisplayClaims>>,
    user_token: Option<&XTokenResponse<XAUDisplayClaims>>,
    relying_party: &str,
) -> Result<XTokenResponse<XSTSDisplayClaims>, xal::Error> {
    let app_params = get_app_params();
    let mut authenticator = XalAuthenticator::new(app_params, CLIENT_WINDOWS(), "RETAIL".into());
    authenticator
        .get_xsts_token(device_token, title_token, user_token, relying_party)
        .await
}

pub async fn refresh_tokens(
    authenticator: &mut XalAuthenticator,
    live_token: StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>,
) -> Result<TokenStore, Box<dyn std::error::Error>> {
    let ts = Flows::xbox_live_sisu_authorization_flow(authenticator, live_token).await?;
    Ok(ts)
}

/// Load the proof key this service should mint with, from XODUS_PROOF_KEY.
///
/// WHY. An XSTS token is bound to the proof key advertised when it was minted,
/// and only the holder of that key can sign requests carrying the token. When a
/// GDK asks this service for a title-scoped token but signs its own requests,
/// the two must be the same key - otherwise every signed Xbox Live call goes out
/// unsigned (or wrongly signed) and services that check it refuse the caller.
///
/// Format: three lines of 64 lowercase hex chars - x, y, d - the same file the
/// GDK side reads. Only `d` is needed to rebuild the keypair; x and y are
/// present so one file describes the whole key and can be checked by eye.
/// Unset or unreadable => None, and a key is generated as before.
/// Keychain entry holding the proof key, alongside the token store's entries.
const PROOF_KEY_ENTRY: &str = "proof_key";

fn key_from_hex_line(text: &str, what: &str) -> Option<xal::SecretKey> {
    let d = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .nth(2)?;
    let bytes = (0..d.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&d[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .inspect_err(|_| log::warn!("{what}: line 3 is not hex"))
        .ok()?;
    xal::SecretKey::from_slice(&bytes)
        .inspect_err(|e| log::warn!("{what}: not a valid P-256 scalar: {e}"))
        .ok()
}

/// The proof key this service mints tokens with.
///
/// WHY IT LIVES HERE. A token is bound to the proof key advertised when it was
/// minted, and only the holder of that key can sign requests carrying it. When a
/// caller has us mint but signs for itself, the two must be the same key. It has
/// to be OUR key rather than the caller's, because device credentials are minted
/// at startup - before any caller has connected to offer one - and those are
/// bound to it too.
///
/// So it is generated once and kept in the keychain next to the token store. A
/// caller asks for it over the IPC socket (PROOF_KEY_REQUEST), which means
/// neither side needs configuring and the two cannot disagree.
///
/// XODUS_PROOF_KEY still overrides, naming a file of three hex lines - x, y, d -
/// for the case where a DeviceToken already exists that was minted against a
/// specific key, which a freshly generated one would not match.
pub fn service_proof_key() -> Option<xal::SecretKey> {
    if let Ok(path) = std::env::var("XODUS_PROOF_KEY") {
        let text = std::fs::read_to_string(&path)
            .inspect_err(|e| log::warn!("XODUS_PROOF_KEY {path}: {e}"))
            .ok()?;
        log::info!("using host-supplied proof key from {path}");
        return key_from_hex_line(&text, "XODUS_PROOF_KEY");
    }

    let entry = crate::secrets::get_entry(PROOF_KEY_ENTRY)
        .inspect_err(|e| log::warn!("proof key: no keychain entry: {e}"))
        .ok()?;

    match entry.get_secret() {
        Ok(bytes) => xal::SecretKey::from_slice(&bytes)
            .inspect_err(|e| log::warn!("stored proof key is unusable: {e}"))
            .ok(),
        Err(keyring_core::Error::NoEntry) => {
            let key = xal::SecretKey::random(&mut rand08::rngs::OsRng);
            entry
                .set_secret(&key.to_bytes())
                .inspect_err(|e| log::warn!("could not store the proof key: {e}"))
                .ok()?;
            log::info!("generated a proof key and stored it in the keychain");
            Some(key)
        }
        Err(e) => {
            log::warn!("proof key: keychain read failed: {e}");
            None
        }
    }
}

/// The same key as x, y, d - 64 lowercase hex chars each, the field order and
/// width of a Windows BCRYPT_ECCPRIVATE_BLOB, so a caller can import it whole.
pub fn service_proof_key_xyd() -> Option<(String, String, String)> {
    use p256::elliptic_curve::sec1::ToEncodedPoint;

    let key = service_proof_key()?;
    let point = key.public_key().to_encoded_point(false);
    let hex = |b: &[u8]| b.iter().map(|c| format!("{c:02x}")).collect::<String>();
    Some((hex(point.x()?), hex(point.y()?), hex(&key.to_bytes())))
}

fn host_request_signer() -> Option<xal::RequestSigner> {
    service_proof_key().map(xal::RequestSigner::with_keypair)
}

pub async fn do_sisu(
    client: &Client,
    manager: &TokenManager,
    client_id: &str,
    title_id: i64,
) -> Result<
    (
        XalAuthenticator,
        xal::response::SisuRPSAuthorizationResponse,
        // The device token SISU was authorized with. The title token it returns is
        // bound to THIS device, so an XSTS request that pairs the title token with
        // any other device token is describing two different machines - which is
        // what `title_usage_by_device_exceeded` appears to be complaining about.
        xal::response::DeviceToken,
    ),
    Box<dyn std::error::Error>,
> {
    let Token::Legacy(token) = manager.get_user_sts_token()? else {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "error",
        )));
    };
    let scope = "xboxlive.signin";
    let Token::Legacy(device_token) = manager.get_device_sts_token()? else {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "error",
        )));
    };
    let device_token_resp: soap::RequestSecurityTokenResponse =
        crate::api::live::exchange_device_token(
            client,
            device_token.clone(),
            "{28C08266-F973-4AE6-FFE4-409B249F138F}".to_string(),
            "scope=service::user.auth.xboxlive.com::MBI_SSL&api-version=2.0".to_owned(),
            Some(soap::PolicyReference::token_broker()),
        )
        .await?;

    let Token::Compact(ms_device_token) = device_token_resp.into() else {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "error",
        )));
    };

    let user_token = crate::api::live::exchange_user_token(
        client,
        token,
        "USERNAME".to_string(),
        device_token,
        None,
        Some("Silent".to_string()),
        client_id.to_string(),
        &[
            (
                format!("scope={scope}&api-version=2.0&clientid={client_id}"),
                Some(soap::PolicyReference::token_broker()),
            ),
            ("http://Passport.NET/tb".to_string(), None),
        ],
    )
    .await?;

    let ExchangeUserTokenOutcome::Issued(
        soap::BodyContent::RequestSecurityTokenResponseCollection(mut collection),
    ) = user_token
    else {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "error",
        )));
    };

    if let Some(sts) = collection.security_tokens.pop() {
        let address = sts.applies_to.endpoint_reference.address.clone();
        let sts: Token = sts.into();
        let address = if let Token::Legacy(legacy) = &sts {
            legacy.key_name.clone().unwrap_or(address)
        } else {
            address
        };
        if let Err(err) = manager.save_user_token(address, sts) {
            log::warn!("Failed to persist refreshed STS token: {err}");
        }
    }
    let token: soap::RequestSecurityTokenResponse = collection.security_tokens.remove(0);
    let token: Token = token.into();
    let Token::Compact(user_token) = token else {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "error",
        )));
    };

    let mut auth = XalAuthenticator::new(
        XalAppParameters {
            client_id: client_id.to_owned(),
            title_id: Some(title_id.to_string()),
            auth_scopes: vec![],
            redirect_uri: None,
            client_secret: None,
        },
        xal::XalClientParameters {
            user_agent: "XAL GRTS 2025.11.20251105.000".to_string(),
            device_type: DeviceType::WIN32,
            client_version: "10.0.22621".to_string(),
            query_display: String::new(),
        },
        "RETAIL".to_owned(),
    );

    // Before any token is requested: the device token and the XSTS request both
    // embed the proof key, so it has to be settled first.
    if let Some(signer) = host_request_signer() {
        auth.set_request_signer(signer);
    }

    let data = auth
        .get_device_token_rps(ms_device_token.to_owned())
        .await?;
    let resp = auth
        .sisu_authorize_rps(&user_token, &data.token, None)
        .await
        .expect("ok");
    Ok((auth, resp, data))
}

#[ignore]
#[tokio::test]
async fn test_minecraft_win_auth() {
    let client = reqwest::Client::new();
    crate::secrets::init_secrets().expect("Unable to initialize credentials");
    let tokens = TokenManager::with_keychain_and_memory();

    let (_, resp, _) = do_sisu(&client, &tokens, "0000000040159362", 896928775)
        .await
        .expect("ok");

    println!("title {}", resp.title_token.token);
    println!("user {}", resp.user_token.token);
    println!("webpage {}", resp.web_page);
}

/// Does SISU issue a TITLE TOKEN for Forza Motorsport's own MSA AppId?
///
/// This is the whole remaining question for FM on Linux. Its Logon to
/// gameservices.fm.forzamotorsport.net returns a blank 403, and that endpoint is
/// credential-opaque - it answers identically for a valid token, a junk token and
/// no token at all - so the client cannot learn anything from it. The one
/// measured difference from a working Windows boot is that Windows gets a
/// different uhs per relying party, which is the signature of a TITLE claim, and
/// neither Xodus's IPC nor WineGDK's IUser can produce one today.
///
/// Before either is worth implementing, Microsoft has to be willing to issue the
/// token at all. An earlier probe of the CLASSIC route
/// (title.auth.xboxlive.com/title/authenticate, with a self-minted device token)
/// returned 403 for this AppId with the request shape exhausted by differential
/// testing. SISU is a DIFFERENT route and is the one a real GDK uses - and it is
/// the one this crate already implements and the author has working for Minecraft
/// - so it deserves its own answer rather than an assumption.
///
/// FM: MSAAppId 000000004CC9D265, TitleId 0x6DD4E56D = 1842668909.
#[ignore]
#[tokio::test]
async fn test_forza_motorsport_sisu() {
    let client = reqwest::Client::new();
    crate::secrets::init_secrets().expect("Unable to initialize credentials");
    let tokens = TokenManager::with_keychain_and_memory();

    match do_sisu(&client, &tokens, "000000004CC9D265", 1842668909).await {
        Ok((_, resp, _)) => {
            println!("FM-SISU: OK");
            println!("FM-SISU: title token len {}", resp.title_token.token.len());
            println!("FM-SISU: user  token len {}", resp.user_token.token.len());
        }
        Err(e) => println!("FM-SISU: REFUSED: {e:?}"),
    }
}

/// Follow the SISU title token through to an XSTS for FORZA'S relying party, and
/// write the finished `XBL3.0 x=<uhs>;<token>` header out so it can be replayed
/// against the real Logon endpoint. This is the end-to-end test of the title
/// claim WITHOUT touching the game or writing any IPC plumbing: if the replay
/// stops returning 403, the plumbing is worth building; if it does not, nothing
/// was lost.
///
/// The measured tell to look for is the USERHASH: Windows gets a different uhs
/// for the Forza relying party than for xboxlive.com in the same boot, while
/// every token we have minted so far (UserToken alone, and UserToken+DeviceToken)
/// has produced one uniform uhs across every relying party.
#[ignore]
#[tokio::test]
async fn test_forza_xsts_with_title_token() {
    let client = reqwest::Client::new();
    crate::secrets::init_secrets().expect("Unable to initialize credentials");
    let tokens = TokenManager::with_keychain_and_memory();

    let (_, resp, _) = do_sisu(&client, &tokens, "000000004CC9D265", 1842668909)
        .await
        .expect("sisu");
    println!(
        "FM-XSTS: sisu ok, title={} user={}",
        resp.title_token.token.len(),
        resp.user_token.token.len()
    );
    println!(
        "FM-XSTS: sisu authorization_token uhs = {}",
        resp.authorization_token.userhash()
    );

    for rp in [
        "http://xboxliveauth.forzamotorsport.net/",
        "http://xboxlive.com",
    ] {
        match get_xsts_token(None, Some(&resp.title_token), Some(&resp.user_token), rp).await {
            Ok(x) => {
                println!(
                    "FM-XSTS: {rp} -> uhs {} tokenlen {}",
                    x.userhash(),
                    x.token.len()
                );
                if rp.contains("forzamotorsport") {
                    let hdr = format!("XBL3.0 x={};{}", x.userhash(), x.token);
                    std::fs::write("/tmp/fm_title_xsts.txt", &hdr).expect("write");
                    println!(
                        "FM-XSTS: header written to /tmp/fm_title_xsts.txt ({} chars)",
                        hdr.len()
                    );
                }
            }
            Err(e) => println!("FM-XSTS: {rp} -> FAILED {e:?}"),
        }
    }
}

#[ignore]
#[tokio::test]
async fn test_forza_title_endpoints() {
    use xal::extensions::SigningReqwestBuilder;

    let client = reqwest::Client::new();
    crate::secrets::init_secrets().expect("Unable to initialize credentials");
    let tokens = TokenManager::with_keychain_and_memory();

    // keep the authenticator — you threw it away as `_` before, and it holds the signer
    let (auth, resp, _) = do_sisu(&client, &tokens, "000000004CC9D265", 1842668909)
        .await
        .expect("SISU failed");

    println!("uhs        = {}", resp.authorization_token.userhash());
    println!("xsts expiry= {}", resp.authorization_token.not_after);

    let auth_header = resp.authorization_token.authorization_header_value();
    let mut signer = auth.request_signer();

    let response = client
        .get("https://title.mgt.xboxlive.com/titles/current/endpoints")
        .query(&[("type", "1")])
        .header("Authorization", &auth_header)
        .header("x-xbl-contract-version", "1")
        .header("Accept", "application/json")
        .sign(&mut signer, None)
        .await
        .expect("failed to sign request")
        .send()
        .await
        .expect("request failed");

    println!("STATUS: {}", response.status());
    for (k, v) in response.headers() {
        println!("  {k}: {v:?}");
    }
    println!("BODY:\n{}", response.text().await.unwrap_or_default());
}

/// The full chain, all on ONE authenticator so the proof key matches throughout:
///   device token (XAD) + SISU title token (XAT) + user token (XAU)
///     -> XSTS for Forza's relying party
/// The previous attempt passed device=None and xsts.auth answered 400, which is
/// its documented behaviour when a TitleToken arrives without a DeviceToken.
/// Reusing do_sisu's returned authenticator matters: a fresh one generates a new
/// proof key, and a device token bound to a different key is the same 400.
#[ignore]
#[tokio::test]
async fn test_forza_xsts_full_chain() {
    let client = reqwest::Client::new();
    crate::secrets::init_secrets().expect("Unable to initialize credentials");
    let tokens = TokenManager::with_keychain_and_memory();

    let (mut auth, resp, _) = do_sisu(&client, &tokens, "000000004CC9D265", 1842668909)
        .await
        .expect("sisu");
    println!(
        "FM-FULL: sisu ok (title {} chars)",
        resp.title_token.token.len()
    );

    let xad = auth.get_device_token().await.expect("device token");
    println!("FM-FULL: device token ok ({} chars)", xad.token.len());

    for rp in [
        "http://xboxliveauth.forzamotorsport.net/",
        "http://xboxlive.com",
    ] {
        match auth
            .get_xsts_token(
                Some(&xad),
                Some(&resp.title_token),
                Some(&resp.user_token),
                rp,
            )
            .await
        {
            Ok(x) => {
                println!(
                    "FM-FULL: {rp}\n         uhs {}  tokenlen {}",
                    x.userhash(),
                    x.token.len()
                );
                if rp.contains("forzamotorsport") {
                    let hdr = format!("XBL3.0 x={};{}", x.userhash(), x.token);
                    std::fs::write("/tmp/fm_title_xsts.txt", &hdr).expect("write");
                    println!(
                        "         header -> /tmp/fm_title_xsts.txt ({} chars)",
                        hdr.len()
                    );
                }
            }
            Err(e) => println!("FM-FULL: {rp} -> FAILED {e:?}"),
        }
    }
}

/// ONE DEVICE IDENTITY end to end.
///
/// The previous attempt paired SISU's title token with a device token minted
/// separately by `get_device_token()`, whose identity is
/// `XalAuthenticator::device_id = Uuid::new_v4()` - a different machine every
/// instance - and xsts.auth answered
/// `401 XSTS error="title_usage_by_device_exceeded"` (XErr 2148916254).
///
/// `do_sisu` already mints the right one via `get_device_token_rps` and then
/// drops it on the floor. This uses THAT device token, so the device token, the
/// title token bound to it, and the user token all describe one device.
#[ignore]
#[tokio::test]
async fn test_forza_xsts_one_device() {
    let client = reqwest::Client::new();
    crate::secrets::init_secrets().expect("Unable to initialize credentials");
    let tokens = TokenManager::with_keychain_and_memory();

    let (mut auth, resp, device) = do_sisu(&client, &tokens, "000000004CC9D265", 1842668909)
        .await
        .expect("sisu");
    println!(
        "FM-ONE: sisu ok - title {} user {} device {} (the SISU device)",
        resp.title_token.token.len(),
        resp.user_token.token.len(),
        device.token.len()
    );

    for rp in [
        "http://xboxliveauth.forzamotorsport.net/",
        "http://xboxlive.com",
    ] {
        match auth
            .get_xsts_token(
                Some(&device),
                Some(&resp.title_token),
                Some(&resp.user_token),
                rp,
            )
            .await
        {
            Ok(x) => {
                println!(
                    "FM-ONE: {rp}\n        uhs {}  tokenlen {}",
                    x.userhash(),
                    x.token.len()
                );
                let hdr = format!("XBL3.0 x={};{}", x.userhash(), x.token);
                let path = if rp.contains("forzamotorsport") {
                    "/tmp/fm_title_xsts.txt"
                } else {
                    // For title.mgt.xboxlive.com/titles/current/endpoints - the
                    // TITLE-SCOPED discovery document, which needs title auth.
                    "/tmp/fm_xbl_xsts.txt"
                };
                std::fs::write(path, &hdr).expect("write");
                println!("        header -> {path} ({} chars)", hdr.len());
            }
            Err(e) => println!("FM-ONE: {rp} -> FAILED {e:?}"),
        }
    }
}
