//! UltraViewer's captcha page (server command 60). The person solves it in an
//! embedded browser; the page reports the token through
//! `window.external.FromJS_CaptchaCorrect(_, token)`, the scripting object the
//! Windows client's browser control exposes, which is bridged here.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use ultraviewer_protocol::CaptchaChallenge;
use webkit6::prelude::*;

use crate::i18n::{language, tr, Language};

const HANDLER: &str = "linuxviewer";

/// Stands in for the Windows scripting object: every method call is posted
/// to the app as `{name, args}`.
const BRIDGE: &str = r#"(function () {
  var post = function (name, args) {
    window.webkit.messageHandlers.linuxviewer.postMessage(JSON.stringify({
      name: String(name),
      args: Array.prototype.map.call(args, function (a) { return a == null ? "" : String(a); })
    }));
  };
  var external = new Proxy({}, {
    get: function (_, name) { return typeof name === "string" ? function () { post(name, arguments); } : undefined; },
    has: function () { return true; }
  });
  try { Object.defineProperty(window, "external", { value: external, configurable: true }); } catch (e) {}
})();"#;

#[derive(serde::Deserialize)]
struct BridgeCall {
    name: String,
    args: Vec<String>,
}

pub fn page_language() -> &'static str {
    match language() {
        Language::Vietnamese => "vi",
        Language::English => "en",
    }
}

/// Shows the captcha page for `challenge`; `solved` receives the token once.
pub fn present(parent: &impl IsA<gtk::Window>, challenge: &CaptchaChallenge, solved: impl Fn(String) + 'static) -> adw::Window {
    let content = webkit6::UserContentManager::new();
    content.register_script_message_handler(HANDLER, None);
    content.add_script(&webkit6::UserScript::new(
        BRIDGE,
        webkit6::UserContentInjectedFrames::AllFrames,
        webkit6::UserScriptInjectionTime::Start,
        &[],
        &[],
    ));
    let view = webkit6::WebView::builder()
        .user_content_manager(&content)
        .network_session(&webkit6::NetworkSession::new_ephemeral())
        .vexpand(true)
        .hexpand(true)
        .build();

    let hint = gtk::Label::builder()
        .label(tr("UltraViewer needs to verify this computer once before it gives it an ID. Type the characters you see and confirm."))
        .wrap(true)
        .xalign(0.0)
        .margin_start(12)
        .margin_end(12)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    let error = gtk::Label::builder().wrap(true).xalign(0.0).margin_start(12).margin_end(12).margin_bottom(8).visible(false).build();
    error.add_css_class("error");
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&hint);
    body.append(&error);
    body.append(&view);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&body));
    let window = adw::Window::builder()
        .title(tr("Verify this computer"))
        .modal(true)
        .transient_for(parent)
        .default_width(520)
        .default_height(680)
        .content(&toolbar)
        .build();

    let done = Rc::new(Cell::new(false));
    let weak = window.downgrade();
    content.connect_script_message_received(Some(HANDLER), move |_, value| {
        let Ok(call) = serde_json::from_str::<BridgeCall>(&value.to_str()) else { return };
        log::info!("captcha page called {} with {} argument(s)", call.name, call.args.len());
        if call.name != "FromJS_CaptchaCorrect" || done.get() {
            return;
        }
        let Some(token) = call.args.get(1).map(|t| t.trim().to_string()) else { return };
        if !CaptchaChallenge::valid_token(&token) {
            log::warn!("captcha token rejected ({} bytes)", token.len());
            return;
        }
        done.set(true);
        solved(token);
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    view.connect_load_failed(move |_, _, uri, failure| {
        log::warn!("captcha page {uri} failed: {failure}");
        error.set_text(&tr("The UltraViewer verification page could not be loaded. Check the connection and try again."));
        error.set_visible(true);
        false
    });
    // Links that ask for a new window stay in this one.
    view.connect_create(|view, action| {
        if let Some(uri) = action.clone().request().and_then(|r| r.uri()) {
            view.load_uri(&uri);
        }
        None
    });
    view.load_uri(&challenge.url(page_language()));
    window.present();
    window
}
