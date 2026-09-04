use crate::{
    DEFAULT_SYNC_CHUNK_BYTES, PullOptions, SkillleafError, SkillleafResult, doctor,
    list_sync_versions, load_catalog, publish_snapshot, pull_snapshot, rollback_sync_snapshot,
    sync_status,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::PathBuf;

const REQUEST_HEADER: &str = "x-skillleaf-request";
const MAX_API_BODY_BYTES: usize = 16 * 1024;

pub fn serve_ui(bind: SocketAddr) -> SkillleafResult<()> {
    if !bind.ip().is_loopback() {
        return Err(SkillleafError::Storage(
            "the REST dashboard may only bind to a loopback address".into(),
        ));
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(storage)?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(bind).await.map_err(storage)?;
        let address = listener.local_addr().map_err(storage)?;
        println!("Skill-Leaf dashboard: http://{address}");
        axum::serve(listener, router()).await.map_err(storage)
    })
}

fn router() -> Router {
    let protected = Router::new()
        .route("/api/v1/sync/status", post(status))
        .route("/api/v1/sync/pull", post(pull))
        .route("/api/v1/sync/publish", post(publish))
        .route("/api/v1/sync/versions", post(versions))
        .route("/api/v1/sync/rollback", post(rollback))
        .route("/api/v1/catalog", post(catalog))
        .layer(middleware::from_fn(require_local_request));
    Router::new()
        .route("/", get(index))
        .route("/openapi.json", get(openapi))
        .route("/api/v1/health", get(health))
        .merge(protected)
        .layer(DefaultBodyLimit::max(MAX_API_BODY_BYTES))
}

async fn index() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Html(INDEX_HTML),
    )
}

async fn openapi() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        OPENAPI,
    )
}

async fn health() -> Json<Value> {
    Json(
        json!({"schema":"skillleaf.web-health.v1","status":"ready","version":env!("CARGO_PKG_VERSION")}),
    )
}

#[derive(Deserialize)]
struct StatusRequest {
    remote: PathBuf,
    destination: PathBuf,
    domain: String,
}

#[derive(Deserialize)]
struct PullRequest {
    remote: PathBuf,
    destination: PathBuf,
    registry: PathBuf,
    domain: String,
    expected_snapshot: Option<String>,
    #[serde(default)]
    trust_remote: bool,
    #[serde(default = "default_true")]
    allow_offline_fallback: bool,
}

#[derive(Deserialize)]
struct PublishRequest {
    catalog: PathBuf,
    remote: PathBuf,
    #[serde(default = "default_chunk_bytes")]
    chunk_bytes: usize,
}

#[derive(Deserialize)]
struct CatalogRequest {
    catalog: PathBuf,
}

#[derive(Deserialize)]
struct VersionsRequest {
    destination: PathBuf,
    domain: String,
}

#[derive(Deserialize)]
struct RollbackRequest {
    destination: PathBuf,
    registry: PathBuf,
    domain: String,
    snapshot_id: String,
}

async fn status(Json(request): Json<StatusRequest>) -> Response {
    match sync_status(&request.remote, &request.destination, &request.domain) {
        Ok(value) => (StatusCode::OK, Json(json!(value))).into_response(),
        Err(error) => api_error(StatusCode::UNPROCESSABLE_ENTITY, error),
    }
}

async fn pull(Json(request): Json<PullRequest>) -> Response {
    let options = PullOptions {
        expected_snapshot: request.expected_snapshot,
        trust_remote: request.trust_remote,
        allow_offline_fallback: request.allow_offline_fallback,
    };
    match pull_snapshot(
        &request.remote,
        &request.destination,
        &request.registry,
        &request.domain,
        &options,
    ) {
        Ok(value) => (StatusCode::OK, Json(json!(value))).into_response(),
        Err(error) => api_error(StatusCode::UNPROCESSABLE_ENTITY, error),
    }
}

async fn publish(Json(request): Json<PublishRequest>) -> Response {
    match publish_snapshot(&request.catalog, &request.remote, request.chunk_bytes) {
        Ok(value) => (StatusCode::OK, Json(json!(value))).into_response(),
        Err(error) => api_error(StatusCode::UNPROCESSABLE_ENTITY, error),
    }
}

async fn catalog(Json(request): Json<CatalogRequest>) -> Response {
    match load_catalog(&request.catalog).and_then(|catalog| {
        doctor(&catalog)?;
        Ok(catalog)
    }) {
        Ok(value) => (StatusCode::OK, Json(json!(value))).into_response(),
        Err(error) => api_error(StatusCode::UNPROCESSABLE_ENTITY, error),
    }
}

async fn versions(Json(request): Json<VersionsRequest>) -> Response {
    match list_sync_versions(&request.destination, &request.domain) {
        Ok(value) => (StatusCode::OK, Json(json!({"versions": value}))).into_response(),
        Err(error) => api_error(StatusCode::UNPROCESSABLE_ENTITY, error),
    }
}

async fn rollback(Json(request): Json<RollbackRequest>) -> Response {
    match rollback_sync_snapshot(
        &request.destination,
        &request.registry,
        &request.domain,
        &request.snapshot_id,
    ) {
        Ok(value) => (StatusCode::OK, Json(json!(value))).into_response(),
        Err(error) => api_error(StatusCode::UNPROCESSABLE_ENTITY, error),
    }
}

async fn require_local_request(request: Request, next: Next) -> Response {
    if has_local_request_proof(request.headers()) {
        next.run(request).await
    } else {
        api_error_message(StatusCode::FORBIDDEN, "missing local request proof")
    }
}

fn has_local_request_proof(headers: &HeaderMap) -> bool {
    headers
        .get(REQUEST_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "1")
}

fn api_error(status: StatusCode, error: SkillleafError) -> Response {
    api_error_message(status, &error.to_string())
}

fn api_error_message(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({"schema":"skillleaf.web-error.v1","error":message})),
    )
        .into_response()
}

fn storage(error: impl std::fmt::Display) -> SkillleafError {
    SkillleafError::Storage(error.to_string())
}
const fn default_chunk_bytes() -> usize {
    DEFAULT_SYNC_CHUNK_BYTES
}
const fn default_true() -> bool {
    true
}

const OPENAPI: &str = r###"{"openapi":"3.1.0","info":{"title":"Skill-Leaf Local API","version":"0.2.0"},"servers":[{"url":"http://127.0.0.1:8787"}],"paths":{"/api/v1/health":{"get":{"responses":{"200":{"description":"Ready"}}}},"/api/v1/catalog":{"post":{"responses":{"200":{"description":"Verified catalogue"},"403":{"description":"Missing local request proof"},"400":{"description":"Invalid input"}}}},"/api/v1/sync/status":{"post":{"responses":{"200":{"description":"Sync status"},"403":{"description":"Missing local request proof"},"400":{"description":"Invalid input"}}}},"/api/v1/sync/versions":{"post":{"responses":{"200":{"description":"Verified versions"},"403":{"description":"Missing local request proof"},"400":{"description":"Invalid input"}}}},"/api/v1/sync/rollback":{"post":{"responses":{"200":{"description":"Rollback receipt"},"403":{"description":"Missing local request proof"},"400":{"description":"Invalid input"}}}},"/api/v1/sync/pull":{"post":{"responses":{"200":{"description":"Pull receipt"},"403":{"description":"Missing local request proof"},"400":{"description":"Invalid input"}}}},"/api/v1/sync/publish":{"post":{"responses":{"200":{"description":"Published manifest"},"403":{"description":"Missing local request proof"},"400":{"description":"Invalid input"}}}}}}"###;

const INDEX_HTML: &str = r###"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Skill-Leaf</title>
<style>
:root{color-scheme:light;--ink:#18251e;--muted:#607068;--line:#dce5df;--paper:#f7faf8;--leaf:#176b45;--leaf2:#0f5235;--soft:#e8f4ed;--warn:#8a4d13;--error:#a33232;--shadow:0 18px 50px rgba(24,55,38,.08);font-family:Inter,ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}
*{box-sizing:border-box}body{margin:0;background:radial-gradient(circle at 85% 0,#e4f3e9 0,transparent 32rem),var(--paper);color:var(--ink);min-height:100vh}button,input{font:inherit}button{min-height:44px;border-radius:12px;border:1px solid transparent;padding:.7rem 1rem;font-weight:700;cursor:pointer;display:inline-flex;align-items:center;justify-content:center;gap:.55rem}button:focus-visible,input:focus-visible{outline:3px solid #83bda1;outline-offset:2px}.primary{background:var(--leaf);color:#fff}.primary:hover{background:var(--leaf2)}.secondary{background:#fff;border-color:var(--line);color:var(--ink)}.soft{background:var(--soft);color:var(--leaf2)}button:disabled{cursor:not-allowed;opacity:.58}svg{width:1.15rem;height:1.15rem;stroke:currentColor;fill:none;stroke-width:1.8;stroke-linecap:round;stroke-linejoin:round}
.shell{width:min(1180px,calc(100% - 2rem));margin:auto;padding:2rem 0 4rem}.topbar,.section-head{display:flex;align-items:center;justify-content:space-between;gap:1rem}.topbar{margin-bottom:2.2rem}.brand{display:flex;align-items:center;gap:.85rem}.mark{width:42px;height:42px;border-radius:13px;background:var(--leaf);color:#fff;display:grid;place-items:center}.mark svg{width:24px;height:24px}.brand strong{display:block;font-size:1.05rem}.brand span,.hint{color:var(--muted);font-size:.8rem}.health{display:flex;align-items:center;gap:.5rem;color:var(--muted);font-size:.86rem}.dot{width:.55rem;height:.55rem;border-radius:50%;background:#3f9b69;box-shadow:0 0 0 4px #dff2e7}
.hero{display:grid;grid-template-columns:1.05fr .95fr;gap:1.3rem;align-items:stretch}.intro,.panel{background:rgba(255,255,255,.9);border:1px solid rgba(209,224,215,.9);border-radius:22px;box-shadow:var(--shadow)}.intro{padding:clamp(1.6rem,4vw,3.2rem);display:flex;flex-direction:column;justify-content:space-between;min-height:410px}.eyebrow{color:var(--leaf);font-size:.78rem;font-weight:800;letter-spacing:.1em;text-transform:uppercase}.intro h1{font-size:clamp(2.2rem,5vw,4.6rem);line-height:.96;letter-spacing:-.055em;margin:.65rem 0 1.2rem;max-width:11ch}.intro p{color:var(--muted);line-height:1.65;max-width:57ch}.assurance{display:flex;gap:1rem;flex-wrap:wrap;margin-top:2rem}.assurance span{display:flex;align-items:center;gap:.4rem;font-size:.8rem;color:var(--muted)}
.panel{padding:1.35rem}.panel h2{font-size:1rem;margin:.1rem 0 1.2rem}.fields{display:grid;gap:.85rem}.field label{display:block;font-size:.78rem;font-weight:750;margin:0 0 .38rem}.field input,.search{width:100%;min-height:44px;border:1px solid var(--line);border-radius:11px;background:#fbfdfc;padding:.68rem .8rem;color:var(--ink)}.two{display:grid;grid-template-columns:1fr 1fr;gap:.75rem}.actions{display:grid;grid-template-columns:repeat(2,1fr);gap:.65rem;margin-top:1.2rem}.actions .primary{grid-column:1/-1}.storage-note{margin:.38rem 0 0;color:var(--muted);font-size:.73rem;line-height:1.45}.status{min-height:72px;border-radius:13px;background:var(--soft);padding:.9rem 1rem;margin-top:1rem;color:var(--leaf2);font-size:.86rem;line-height:1.45}.status[data-kind=error]{background:#f9e9e9;color:var(--error)}.status[data-kind=warn]{background:#fff1df;color:var(--warn)}.status strong{display:block;margin-bottom:.2rem}.progress{height:3px;background:#cfe4d8;overflow:hidden;border-radius:3px;margin-top:.7rem;visibility:hidden}.busy .progress{visibility:visible}.progress:after{content:"";display:block;width:38%;height:100%;background:var(--leaf);animation:travel 1.4s linear infinite}@keyframes travel{from{transform:translateX(-100%)}to{transform:translateX(365%)}}.meta{display:grid;grid-template-columns:repeat(3,1fr);gap:.7rem;margin-top:1.3rem}.metric{border:1px solid var(--line);border-radius:14px;padding:.9rem;background:#fff}.metric b{display:block;font-size:1.08rem;margin-bottom:.25rem}.metric span{font-size:.72rem;color:var(--muted)}
.manager{display:grid;grid-template-columns:1.35fr .65fr;gap:1.3rem;margin-top:1.3rem}.manager .panel{min-height:360px}.section-head{margin-bottom:1rem}.section-head h2{margin:0}.counts{display:flex;gap:.4rem}.pill,.badge{border-radius:999px;padding:.27rem .56rem;font-size:.69rem;font-weight:800}.pill{background:var(--soft);color:var(--leaf2)}.toolbar{display:grid;grid-template-columns:1fr auto;gap:.65rem;margin-bottom:.8rem}.list{display:grid;gap:.55rem}.row{display:grid;grid-template-columns:auto 1fr auto;align-items:center;gap:.8rem;padding:.78rem;border:1px solid var(--line);border-radius:13px;background:#fff}.row-icon{width:34px;height:34px;border-radius:10px;background:var(--soft);color:var(--leaf);display:grid;place-items:center;font-weight:900}.row-copy{min-width:0}.row-copy strong,.row-copy span{display:block;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}.row-copy span{font-size:.76rem;color:var(--muted);margin-top:.18rem}.badges{display:flex;justify-content:flex-end;gap:.35rem;flex-wrap:wrap}.badge{background:#eef2ef;color:var(--muted)}.badge.trusted{background:#dff2e7;color:var(--leaf2)}.version-row{grid-template-columns:1fr auto}.version-row button{min-height:36px;padding:.42rem .65rem}.empty{border:1px dashed var(--line);border-radius:13px;padding:2rem 1rem;text-align:center;color:var(--muted);font-size:.86rem}
@media(max-width:850px){.hero,.manager{grid-template-columns:1fr}.intro{min-height:330px}.shell{width:min(100% - 1rem,680px);padding-top:1rem}.intro,.panel{border-radius:18px}.meta{grid-template-columns:1fr}.health span:last-child{display:none}}@media(max-width:500px){.two,.actions,.toolbar{grid-template-columns:1fr}.actions .primary{grid-column:auto}.intro{padding:1.4rem}.panel{padding:1rem}.row{grid-template-columns:auto 1fr}.badges{grid-column:1/-1;justify-content:flex-start}}
</style></head><body><main class="shell"><header class="topbar"><div class="brand"><div class="mark" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="M19 4C12 4 6 7.2 6 13.2c0 3.2 2.2 5.8 5.5 5.8C18 19 20 11 19 4Z"/><path d="M5 20c2.8-5.7 6.5-8.8 11-10.4"/></svg></div><div><strong>Skill-Leaf</strong><span>Local skill federation</span></div></div><div class="health"><i class="dot"></i><span id="health">Checking local service</span></div></header>
<section class="hero"><article class="intro"><div><span class="eyebrow">Private by default</span><h1>Your skills. In sync.</h1><p>Manage skills and commands, save verified backups, and roll back safely without exposing your catalogue to a hosted service.</p></div><div class="assurance"><span><svg viewBox="0 0 24 24"><path d="m5 12 4 4L19 6"/></svg>Verified versions</span><span><svg viewBox="0 0 24 24"><path d="M12 3 4.5 6v5.5c0 4.7 3.2 8 7.5 9.5 4.3-1.5 7.5-4.8 7.5-9.5V6L12 3Z"/><path d="m9 12 2 2 4-4"/></svg>Trust-aware</span><span><svg viewBox="0 0 24 24"><path d="M4 12a8 8 0 0 1 13.6-5.7L20 9"/><path d="M20 4v5h-5M20 12a8 8 0 0 1-13.6 5.7L4 15"/><path d="M4 20v-5h5"/></svg>Atomic activation</span></div></article>
<section class="panel" aria-labelledby="workspace-title"><h2 id="workspace-title">Sync workspace</h2><form id="sync-form"><div class="fields"><div class="field"><label for="remote">Backup / bucket location</label><input id="remote" name="remote" value="./skillleaf-remote" required><p class="storage-note">Local folder or mounted Cloudflare R2, Amazon S3, Azure Blob, or OneDrive location.</p></div><div class="field"><label for="destination">Local Skill-Leaf data</label><input id="destination" name="destination" value="./skillleaf-data" required></div><div class="two"><div class="field"><label for="domain">Domain</label><input id="domain" name="domain" value="personal" required pattern="[A-Za-z0-9][A-Za-z0-9._-]*"></div><div class="field"><label for="registry">Domain registry</label><input id="registry" name="registry" value="./skillleaf-domains.json" required></div></div><div class="field"><label for="catalog">Current catalogue</label><input id="catalog" name="catalog" value="./skillleaf.json" required></div></div>
<div class="actions"><button class="primary" id="status-button" type="button"><svg viewBox="0 0 24 24"><path d="M4 12a8 8 0 1 1 2.3 5.7"/><path d="M4 18v-5h5"/></svg>Refresh library</button><button class="secondary" id="pull-button" type="button"><svg viewBox="0 0 24 24"><path d="M12 3v12m-5-5 5 5 5-5"/><path d="M5 21h14"/></svg>Pull update</button><button class="soft" id="publish-button" type="button"><svg viewBox="0 0 24 24"><path d="M12 21V9m-5 5 5-5 5 5"/><path d="M5 3h14"/></svg>Save &amp; back up</button><button class="secondary" id="share-button" type="button"><svg viewBox="0 0 24 24"><circle cx="18" cy="5" r="2.5"/><circle cx="6" cy="12" r="2.5"/><circle cx="18" cy="19" r="2.5"/><path d="m8.2 10.8 7.6-4.5M8.2 13.2l7.6 4.5"/></svg>Share location</button></div></form><div class="status" id="result" role="status" aria-live="polite"><strong>Ready</strong><span>Refresh to inspect this workspace.</span><div class="progress" aria-hidden="true"></div></div><div class="meta"><div class="metric"><b id="remote-state">—</b><span>Remote snapshot</span></div><div class="metric"><b id="local-state">—</b><span>Active version</span></div><div class="metric"><b id="verified-state">—</b><span>Local verification</span></div></div></section></section>
<section class="manager"><section class="panel" aria-labelledby="library-title"><div class="section-head"><h2 id="library-title">Current library</h2><div class="counts"><span class="pill" id="skill-count">0 skills</span><span class="pill" id="command-count">0 commands</span></div></div><div class="toolbar"><input class="search" id="library-search" type="search" placeholder="Search skills and commands" aria-label="Search skills and commands"><button class="secondary" id="clear-search" type="button">Clear</button></div><div class="list" id="library-list"><div class="empty">Refresh the library to see current skills and commands.</div></div></section>
<section class="panel" aria-labelledby="versions-title"><div class="section-head"><h2 id="versions-title">Versions</h2><span class="pill" id="version-count">0 saved</span></div><div class="list" id="version-list"><div class="empty">Pulled snapshots appear here with rollback controls.</div></div></section></section></main>
<script>
const form=document.querySelector("#sync-form"),result=document.querySelector("#result"),buttons=[...document.querySelectorAll("button")],library=document.querySelector("#library-list"),versionList=document.querySelector("#version-list");let entries=[];
const values=()=>Object.fromEntries(new FormData(form).entries());function setBusy(on){buttons.forEach(button=>button.disabled=on);result.classList.toggle("busy",on);result.setAttribute("aria-busy",String(on))}function message(title,detail,kind="ok"){result.dataset.kind=kind;result.querySelector("strong").textContent=title;result.querySelector("span").textContent=detail}async function api(path,body){const response=await fetch(path,{method:"POST",headers:{"content-type":"application/json","x-skillleaf-request":"1"},body:JSON.stringify(body)});const data=await response.json();if(!response.ok)throw new Error(data.error||"Request failed");return data}
function empty(container,text){container.replaceChildren();const node=document.createElement("div");node.className="empty";node.textContent=text;container.append(node)}function badge(text,className=""){const node=document.createElement("span");node.className="badge "+className;node.textContent=text;return node}
function renderLibrary(filter=""){const shown=entries.filter(entry=>(entry.name+" "+entry.description+" "+entry.kind).toLowerCase().includes(filter.toLowerCase()));library.replaceChildren();if(!shown.length){empty(library,entries.length?"No matching skills or commands.":"No catalogue entries found.");return}for(const entry of shown){const row=document.createElement("article");row.className="row";const mark=document.createElement("div");mark.className="row-icon";mark.setAttribute("aria-hidden","true");mark.textContent=entry.kind==="command"?">_":"✦";const copy=document.createElement("div");copy.className="row-copy";const name=document.createElement("strong");name.textContent=entry.name;const detail=document.createElement("span");detail.textContent=entry.description||entry.source;copy.append(name,detail);const badges=document.createElement("div");badges.className="badges";const trust=entry.trust||"trusted";badges.append(badge(entry.kind),badge(trust,trust==="trusted"?"trusted":""),badge(entry.content_sha256.slice(0,8)));row.append(mark,copy,badges);library.append(row)}}
function renderVersions(versions){versionList.replaceChildren();document.querySelector("#version-count").textContent=versions.length+" saved";if(!versions.length){empty(versionList,"No local versions are available yet.");return}for(const version of versions){const row=document.createElement("article");row.className="row version-row";const copy=document.createElement("div");copy.className="row-copy";const name=document.createElement("strong");name.textContent=(version.active?"Active · ":"")+version.snapshot_id.slice(0,12);const detail=document.createElement("span");detail.textContent=version.entries+" entries · "+(version.trusted?"trusted":"untrusted");copy.append(name,detail);const action=document.createElement("button");action.type="button";action.className=version.active?"secondary":"soft";action.textContent=version.active?"Current":"Roll back";action.disabled=version.active;action.addEventListener("click",()=>rollback(version.snapshot_id));row.append(copy,action);versionList.append(row)}}
async function refresh(){if(!form.reportValidity())return;setBusy(true);const v=values();try{const [status,catalog,versions]=await Promise.all([api("/api/v1/sync/status",{remote:v.remote,destination:v.destination,domain:v.domain}),api("/api/v1/catalog",{catalog:v.catalog}),api("/api/v1/sync/versions",{destination:v.destination,domain:v.domain})]);entries=catalog.entries;const skills=entries.filter(x=>x.kind==="skill").length,commands=entries.filter(x=>x.kind==="command").length;document.querySelector("#skill-count").textContent=skills+" skill"+(skills===1?"":"s");document.querySelector("#command-count").textContent=commands+" command"+(commands===1?"":"s");renderLibrary(document.querySelector("#library-search").value);renderVersions(versions.versions);document.querySelector("#remote-state").textContent=status.remote_snapshot?.slice(0,10)||"Not found";document.querySelector("#local-state").textContent=status.local_snapshot?.slice(0,10)||"Not synced";document.querySelector("#verified-state").textContent=status.local_verified?"Verified":"Not verified";message(status.update_available?"Update available":"Library refreshed",status.update_available?"Pull the current verified snapshot.":entries.length+" catalogue entries loaded.",status.update_available?"warn":"ok")}catch(error){message("Refresh failed",error.message,"error")}finally{setBusy(false)}}
async function action(path,body,label){if(!form.reportValidity())return;setBusy(true);try{const data=await api(path,body);message(label+" complete",data.snapshot_id?"Snapshot "+data.snapshot_id.slice(0,12):"The request completed successfully.");return data}catch(error){message(label+" failed",error.message,"error")}finally{setBusy(false)}}async function rollback(snapshot){const v=values();const data=await action("/api/v1/sync/rollback",{destination:v.destination,registry:v.registry,domain:v.domain,snapshot_id:snapshot},"Rollback");if(data)refresh()}
async function shareLocation(){const location=values().remote.replace(/\/$/,"")+"/current.json";if(navigator.share){try{await navigator.share({title:"Skill-Leaf snapshot",text:location});message("Share location ready","The system share sheet opened.");return}catch(error){if(error.name==="AbortError"){message("Share cancelled","No changes were made.","warn");return}}}try{await navigator.clipboard.writeText(location);message("Share location ready","The snapshot location was copied.");return}catch{}const remote=document.querySelector("#remote");remote.focus();remote.select();message("Share location ready",location+" — copy this path to share it.","warn")}
document.querySelector("#status-button").addEventListener("click",refresh);document.querySelector("#pull-button").addEventListener("click",async()=>{const v=values();const data=await action("/api/v1/sync/pull",{remote:v.remote,destination:v.destination,registry:v.registry,domain:v.domain,allow_offline_fallback:true},"Pull");if(data)refresh()});document.querySelector("#publish-button").addEventListener("click",()=>{const v=values();action("/api/v1/sync/publish",{catalog:v.catalog,remote:v.remote},"Backup")});document.querySelector("#share-button").addEventListener("click",shareLocation);document.querySelector("#library-search").addEventListener("input",event=>renderLibrary(event.target.value));document.querySelector("#clear-search").addEventListener("click",()=>{document.querySelector("#library-search").value="";renderLibrary()});fetch("/api/v1/health").then(r=>r.json()).then(()=>document.querySelector("#health").textContent="Local service ready").catch(()=>document.querySelector("#health").textContent="Service unavailable");
</script></body></html>"###;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_and_interface_expose_the_expected_contract() {
        assert_eq!(health().await.0["status"], "ready");
        assert!(INDEX_HTML.contains("id=\"status-button\""));
        assert!(INDEX_HTML.contains("<svg"));
        assert!(OPENAPI.contains("/api/v1/sync/pull"));
        assert!(OPENAPI.contains("/api/v1/sync/rollback"));
        assert!(INDEX_HTML.contains("id=\"library-list\""));
        assert!(INDEX_HTML.contains("id=\"share-button\""));
    }

    #[test]
    fn write_proof_accepts_only_the_exact_header_value() {
        let mut valid = HeaderMap::new();
        valid.insert(REQUEST_HEADER, "1".parse().unwrap());
        assert!(has_local_request_proof(&valid));
        let mut invalid = HeaderMap::new();
        invalid.insert(REQUEST_HEADER, "true".parse().unwrap());
        assert!(!has_local_request_proof(&HeaderMap::new()));
        assert!(!has_local_request_proof(&invalid));
    }

    #[test]
    fn public_network_binding_is_rejected() {
        let error = serve_ui("0.0.0.0:8787".parse().unwrap()).unwrap_err();
        assert!(error.to_string().contains("loopback"));
    }
}
