//! Tiny per-request timeout middleware.
//!
//! When a request takes longer than `Timeout::duration` to produce a
//! response, the handler future is dropped and the client gets a
//! `504 Gateway Timeout`. The work the handler started may still finish
//! in the background until the next `.await` point — that's a property
//! of cooperative scheduling, not of this middleware.

use std::future::{Future, ready};
use std::pin::Pin;
use std::time::Duration;

use actix_web::{
    Error, HttpResponse,
    body::{BoxBody, EitherBody},
    dev::{Service, ServiceRequest, ServiceResponse, Transform, forward_ready},
};

#[derive(Clone)]
pub struct Timeout {
    duration: Duration,
}

impl Timeout {
    pub fn new(duration: Duration) -> Self {
        Self { duration }
    }
}

impl<S, B> Transform<S, ServiceRequest> for Timeout
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B, BoxBody>>;
    type Error = Error;
    type Transform = TimeoutMiddleware<S>;
    type InitError = ();
    type Future = std::future::Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(TimeoutMiddleware { service, duration: self.duration }))
    }
}

pub struct TimeoutMiddleware<S> {
    service:  S,
    duration: Duration,
}

impl<S, B> Service<ServiceRequest> for TimeoutMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B, BoxBody>>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let duration = self.duration;
        // Stash the HttpRequest so we can synthesise a 504 ServiceResponse
        // on timeout — the inner future has already consumed `req`.
        let (http_req, payload) = req.into_parts();
        let req = ServiceRequest::from_parts(http_req.clone(), payload);
        let fut = self.service.call(req);
        Box::pin(async move {
            match tokio::time::timeout(duration, fut).await {
                Ok(Ok(resp)) => Ok(resp.map_into_left_body()),
                Ok(Err(e))   => Err(e),
                Err(_) => {
                    log::warn!(
                        "request {} {} exceeded timeout of {} ms",
                        http_req.method(), http_req.path(), duration.as_millis(),
                    );
                    let resp = HttpResponse::GatewayTimeout()
                        .content_type("application/json")
                        .body(r#"{"error":"request timed out"}"#);
                    Ok(ServiceResponse::new(http_req, resp).map_into_right_body())
                }
            }
        })
    }
}
