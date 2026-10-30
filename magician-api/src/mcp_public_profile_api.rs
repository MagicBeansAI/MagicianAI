//! Intentionally public static profiles; no API identity or private files.
use actix_web::{web, HttpResponse};
use magician::magician_v2::mcp_public_profile::{read_public_profile, PUBLIC_PROFILE_PREFIX};
use std::path::PathBuf;

pub struct PublicMcpProfileApi {
    storage_root: PathBuf,
}

impl PublicMcpProfileApi {
    pub fn new(storage_root: PathBuf) -> Self {
        Self { storage_root }
    }
}

pub fn configure_public_mcp_profile_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::resource(format!("{PUBLIC_PROFILE_PREFIX}{{id}}.json"))
            .route(web::get().to(get_public_mcp_profile))
            .route(web::head().to(get_public_mcp_profile)),
    );
}

pub async fn get_public_mcp_profile(
    api: web::Data<PublicMcpProfileApi>,
    id: web::Path<String>,
) -> HttpResponse {
    // Disk work stays off the HTTP worker. All invalid/missing profiles share
    // one response and never reveal local paths or storage errors.
    match web::block(move || read_public_profile(&api.storage_root, &id)).await {
        Ok(Ok(bytes)) => HttpResponse::Ok()
            .content_type("application/json")
            .insert_header(("Cache-Control", "public, max-age=86400, immutable"))
            .insert_header(("X-Content-Type-Options", "nosniff"))
            .body(bytes),
        _ => HttpResponse::NotFound().finish(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{http::StatusCode, test, App};
    use magician::magician_v2::mcp_public_profile::McpPublicProfile;
    use serde_json::json;

    #[actix_web::test]
    async fn public_get_serves_exact_document_without_authentication() {
        let root = tempfile::tempdir().unwrap();
        let profile = McpPublicProfile {
            argument_pointer: "/meta/profile".into(),
            document: json!({"public_capabilities": ["catalog"]}),
        };
        let url = profile
            .publish(root.path(), "https://engine.example.com")
            .unwrap();
        let path = url::Url::parse(&url).unwrap().path().to_owned();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(PublicMcpProfileApi::new(
                    root.path().to_owned(),
                )))
                .configure(configure_public_mcp_profile_routes),
        )
        .await;
        let response =
            test::call_service(&app, test::TestRequest::get().uri(&path).to_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json"
        );
        assert!(response
            .headers()
            .get("cache-control")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("immutable"));
        assert_eq!(
            test::read_body_json::<serde_json::Value, _>(response).await,
            profile.document
        );
        let response = test::call_service(
            &app,
            test::TestRequest::default()
                .method(actix_web::http::Method::HEAD)
                .uri(&path)
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let response =
            test::call_service(&app, test::TestRequest::post().uri(&path).to_request()).await;
        assert!(!response.status().is_success());
    }

    #[actix_web::test]
    async fn public_route_cannot_read_private_or_modified_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env"), "TOKEN=private").unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(PublicMcpProfileApi::new(
                    root.path().to_owned(),
                )))
                .configure(configure_public_mcp_profile_routes),
        )
        .await;
        for path in [
            "/.well-known/magican/mcp-profiles/.env.json".to_owned(),
            format!("/.well-known/magican/mcp-profiles/{}.json", "f".repeat(64)),
            "/.well-known/magican/mcp-profiles/%2e%2e%2f.env.json".to_owned(),
        ] {
            let response =
                test::call_service(&app, test::TestRequest::get().uri(&path).to_request()).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert!(!String::from_utf8_lossy(&test::read_body(response).await).contains("private"));
        }
    }
}
