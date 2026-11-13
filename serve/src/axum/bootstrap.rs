use super::*;

pub(super) async fn bootstrap(
    State(state): State<Arc<SurfaceState>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    if !state.access.admits(query.as_deref(), &state.capabilities) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(host) = admit_headers(&state, &headers, false) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let presented = presented_capability(query.as_deref());
    let Some(capability) = state.capabilities.serving(presented) else {
        return if presented.is_some() {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        }
        .into_response();
    };
    let app_generation = match state.app.generation() {
        Ok(generation) => generation,
        Err(error) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("app unavailable: {error}"),
            )
                .into_response();
        }
    };
    // The socket this names has to be one the asking client can reach, which
    // the published endpoint is not when a remote browser is on the other end
    // of a wildcard bind. It reached this surface somehow; that is the address.
    let mut version = state.version.clone();
    version.app = app_generation.clone();
    let discovery = wire::AppBootstrap {
        endpoint: format!("http://{host}"),
        capability,
        epoch: state.epoch,
        version,
    };
    let Ok(bytes) = wire::encode(&discovery) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        wire::hello::APP_PROTOCOL_VERSION_HEADER,
        HeaderValue::from_str(&discovery.version.app_protocol.to_string())
            .expect("the numeric app protocol version is a valid header value"),
    );
    response.headers_mut().insert(
        wire::hello::APP_GENERATION_HEADER,
        HeaderValue::from_str(app_generation.as_str())
            .expect("the canonical app generation is a valid header value"),
    );
    install_browser_headers(&mut response);
    response
}
