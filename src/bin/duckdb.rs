use actix_web::{middleware, web, App, HttpServer};
use fast_api::duckdb_backend::{db, handlers};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .init();

    let conn = db::load_into_memory("data/us_accidents")
        .expect("Failed to load DB into memory");
    let db = db::init_pool(conn)
        .expect("Failed to create connection pool");

    log::info!("Listening on http://0.0.0.0:8080 (DuckDB backend)");

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(db.clone()))
            .wrap(middleware::Logger::default())
            .service(handlers::health)
            .service(handlers::get_accidents)
            .service(handlers::query_accidents)
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}
