mod errors;
mod handlers;
mod models;
mod store;

use actix_web::{middleware, web, App, HttpServer};
use store::Store;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .init();

    let store = Store::load("data/accidents.parquet")
        .expect("failed to load store");

    let state = web::Data::new(store);

    log::info!("Listening on http://0.0.0.0:8080");

    HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .wrap(middleware::Compress::default())
            .wrap(middleware::Logger::default())
            .service(handlers::health)
            .service(handlers::get_accidents)
            .service(handlers::query_accidents)
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}
