use axum::{extract::State, http::header, response::IntoResponse, routing::get, Router};
use serde::Deserialize;
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(Deserialize)]
struct Config {
    server: ServerConfig,
}

#[derive(Deserialize)]
struct ServerConfig {
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default = "default_width")]
    image_width: u32,
    #[serde(default = "default_height")]
    image_height: u32,
}

fn default_port() -> u16 {
    8080
}
fn default_width() -> u32 {
    800
}
fn default_height() -> u32 {
    480
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            port: default_port(),
            image_width: default_width(),
            image_height: default_height(),
        }
    }
}

impl Config {
    fn load() -> Self {
        match std::fs::read_to_string("config.toml") {
            Ok(content) => toml::from_str(&content).expect("Invalid config.toml"),
            Err(_) => Config {
                server: ServerConfig::default(),
            },
        }
    }
}

fn generate_white_png(width: u32, height: u32) -> Vec<u8> {
    let pixels = vec![255u8; (width * height * 3) as usize];
    let img = image::RgbImage::from_raw(width, height, pixels).expect("Failed to create image");
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("PNG encoding failed");
    buf.into_inner()
}

async fn serve_image(State(png): State<Arc<Vec<u8>>>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], (*png).clone())
}

#[tokio::main]
async fn main() {
    let config = Config::load();
    let png = Arc::new(generate_white_png(
        config.server.image_width,
        config.server.image_height,
    ));

    let app = Router::new()
        .route("/image.png", get(serve_image))
        .with_state(Arc::clone(&png));

    let addr = format!("0.0.0.0:{}", config.server.port);
    let listener = TcpListener::bind(&addr).await.expect("Failed to bind");
    println!("Listening on http://{}", addr);
    axum::serve(listener, app).await.expect("Server error");
}
