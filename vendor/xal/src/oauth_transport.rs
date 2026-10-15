//! OAuth2 4 request types over the maintained reqwest transport.
//!
//! Keep the existing OAuth protocol API and proof-key behavior while removing
//! its default reqwest 0.11 / rustls 0.21 / h2 0.3 dependency graph.

use oauth2::{HttpRequest, HttpResponse};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum AsyncHttpClientError {
    #[error("OAuth HTTP request failed")]
    Request(#[from] reqwest::Error),
    #[error("OAuth HTTP metadata was invalid")]
    Metadata,
    #[error("OAuth response exceeded the size limit")]
    Oversized,
}

pub async fn async_http_client(request: HttpRequest) -> Result<HttpResponse, AsyncHttpClientError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?;
    let method = reqwest::Method::from_bytes(request.method.as_str().as_bytes())
        .map_err(|_| AsyncHttpClientError::Metadata)?;
    let mut builder = client
        .request(method, request.url.as_str())
        .body(request.body);
    for (name, value) in &request.headers {
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let mut response = builder.send().await?;
    let status_code = http02::StatusCode::from_u16(response.status().as_u16())
        .map_err(|_| AsyncHttpClientError::Metadata)?;
    let mut headers = http02::HeaderMap::new();
    for (name, value) in response.headers() {
        headers.append(
            http02::header::HeaderName::from_bytes(name.as_str().as_bytes())
                .map_err(|_| AsyncHttpClientError::Metadata)?,
            http02::header::HeaderValue::from_bytes(value.as_bytes())
                .map_err(|_| AsyncHttpClientError::Metadata)?,
        );
    }
    const MAX_RESPONSE: usize = 1024 * 1024;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > MAX_RESPONSE - body.len() {
            return Err(AsyncHttpClientError::Oversized);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(HttpResponse {
        status_code,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[tokio::test]
    async fn preserves_response_and_never_follows_redirects() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 2048];
            let length = stream.read(&mut request).unwrap();
            assert!(std::str::from_utf8(&request[..length])
                .unwrap()
                .starts_with("POST /token "));
            stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/must-not-follow\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
        });
        let response = async_http_client(HttpRequest {
            url: format!("http://{address}/token").parse().unwrap(),
            method: http02::Method::POST,
            headers: http02::HeaderMap::new(),
            body: Vec::new(),
        })
        .await
        .unwrap();
        assert_eq!(response.status_code, http02::StatusCode::FOUND);
        assert_eq!(
            response.headers["location"],
            "http://127.0.0.1:1/must-not-follow"
        );
        assert_eq!(response.body, b"{}");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn rejects_oversized_oauth_response() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 2048];
            stream.read(&mut request).unwrap();
            let body = vec![b'x'; 1024 * 1024 + 1];
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            let _ = stream.write_all(&body);
        });
        let result = async_http_client(HttpRequest {
            url: format!("http://{address}/token").parse().unwrap(),
            method: http02::Method::GET,
            headers: http02::HeaderMap::new(),
            body: Vec::new(),
        })
        .await;
        assert!(matches!(result, Err(AsyncHttpClientError::Oversized)));
        server.join().unwrap();
    }
}
