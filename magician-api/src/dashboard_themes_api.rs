//! REST handlers for the dashboard theme registry.
//!
//! - `GET /api/magician/v2/dashboard_themes` → list every loaded theme.
//! - `GET /api/magician/v2/dashboard_themes/{id}` → fetch one theme.
//!
//! The registry itself is constructed at startup (see `bin/magician.rs`) and
//! shared across handlers via `web::Data<DashboardThemeRegistry>`. The
//! handlers are scope-agnostic — themes are a process-wide resource — but
//! scope-extensible YAMLs are loaded into the registry at boot, so two
//! scopes with different overlay directories will see different theme sets
//! when they start their own magician process. Live cross-scope themes
//! aren't supported in this cut; revisit once a real use case appears.

use actix_web::{web, HttpResponse};
use serde::Serialize;

use magician::magician_v2::dashboard_themes::{DashboardTheme, DashboardThemeRegistry};

#[derive(Serialize)]
pub struct ListResponse {
    pub themes: Vec<DashboardTheme>,
}

#[derive(Serialize)]
struct NotFound {
    error: String,
}

/// GET `/api/magician/v2/dashboard_themes`
pub async fn list_themes_handler(
    registry: Option<web::Data<DashboardThemeRegistry>>,
) -> HttpResponse {
    let Some(registry) = registry else {
        return HttpResponse::ServiceUnavailable().json(NotFound {
            error: "Dashboard theme registry is not initialized".to_string(),
        });
    };
    HttpResponse::Ok().json(ListResponse {
        themes: registry.list(),
    })
}

/// GET `/api/magician/v2/dashboard_themes/{id}`
pub async fn get_theme_handler(
    registry: Option<web::Data<DashboardThemeRegistry>>,
    path: web::Path<String>,
) -> HttpResponse {
    let Some(registry) = registry else {
        return HttpResponse::ServiceUnavailable().json(NotFound {
            error: "Dashboard theme registry is not initialized".to_string(),
        });
    };
    let id = path.into_inner();
    match registry.get(&id) {
        Some(theme) => HttpResponse::Ok().json(theme),
        None => HttpResponse::NotFound().json(NotFound {
            error: format!("Dashboard theme '{}' not found", id),
        }),
    }
}
