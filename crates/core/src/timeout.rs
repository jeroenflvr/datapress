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
    error::InternalError,
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
        ready(Ok(TimeoutMiddleware {
            service,
            duration: self.duration,
        }))
    }
}

pub struct TimeoutMiddleware<S> {
    service: S,
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
        // We must NOT clone the inner `HttpRequest` before handing the
        // `ServiceRequest` off to the next service: actix's routing layer
        // calls `match_info_mut` (-> `Rc::get_mut().unwrap()`) on the
        // request, which panics if any other clone of the inner Rc is
        // still alive. So we capture just the bits we need for logging
        // by value, and on timeout return an `InternalError` carrying a
        // pre-built 504 response — actix renders it without ever needing
        // the original request.
        let method = req.method().clone();
        let path = req.path().to_owned();
        let fut = self.service.call(req);
        Box::pin(async move {
            match tokio::time::timeout(duration, fut).await {
                Ok(Ok(resp)) => Ok(resp.map_into_left_body()),
                Ok(Err(e)) => Err(e),
                Err(_) => {
                    log::warn!(
                        "request {method} {path} exceeded timeout of {} ms",
                        duration.as_millis(),
                    );
                    let resp = HttpResponse::GatewayTimeout()
                        .content_type("application/json")
                        .body(r#"{"error":"request timed out"}"#);
                    Err(InternalError::from_response("", resp).into())
                }
            }
        })
    }
}
