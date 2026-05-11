use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::Json;
use serde::Serialize;

pub const PLUGIN_TLS_CERT_FILE: &str = "plugin-tls.crt";
pub const PLUGIN_TLS_KEY_FILE: &str = "plugin-tls.key";

#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub detail: String,
}

pub fn error_response(status: StatusCode, detail: impl Into<String>) -> impl IntoResponse {
    (status, Json(MessageResponse { detail: detail.into() }))
}

pub fn ok_response(detail: impl Into<String>) -> impl IntoResponse {
    (StatusCode::OK, Json(MessageResponse { detail: detail.into() }))
}

pub fn get_page(title: &str, config_name: &str, destination_hash: &str) -> Html<String> {
    Html(format!(r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title}</title>
  <style>
    :root {{
      color-scheme: dark;
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }}
    body {{
      margin: 0;
      min-height: 100vh;
      background: #0f172a;
      color: #e2e8f0;
    }}
    .app {{
      max-width: 48rem;
      margin: 0 auto;
      padding: 1.5rem;
    }}
    h1 {{
      margin: 0 0 0.5rem;
      font-size: 1.6rem;
    }}
    .subtitle {{
      color: #94a3b8;
      margin-bottom: 1.5rem;
    }}
    .destination-hash {{
      background: #1e293b;
      border: 1px solid #334155;
      border-radius: 0.5rem;
      padding: 0.75rem 1rem;
      margin-bottom: 1.5rem;
      font-family: "SF Mono", "Consolas", monospace;
      font-size: 0.9rem;
      display: flex;
      align-items: center;
      gap: 0.75rem;
    }}
    .destination-hash .label {{
      color: #94a3b8;
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      white-space: nowrap;
    }}
    .destination-hash .hash {{
      color: #22c55e;
      word-break: break-all;
    }}
    .card {{
      background: #111827;
      border: 1px solid #1f2937;
      border-radius: 1rem;
      padding: 1.25rem;
      margin-bottom: 1rem;
    }}
    .card-title {{
      font-weight: 700;
      margin-bottom: 0.75rem;
      color: #f8fafc;
    }}
    .editor-wrap {{
      position: relative;
    }}
    textarea {{
      width: 100%;
      min-height: 20rem;
      background: #0b1220;
      border: 1px solid #334155;
      border-radius: 0.5rem;
      color: #e2e8f0;
      font-family: "SF Mono", "Consolas", monospace;
      font-size: 0.9rem;
      padding: 0.75rem;
      resize: vertical;
      box-sizing: border-box;
    }}
    textarea:focus {{
      outline: none;
      border-color: #3b82f6;
    }}
    .btn-row {{
      display: flex;
      gap: 0.75rem;
      margin-top: 1rem;
    }}
    .btn {{
      padding: 0.6rem 1.25rem;
      border-radius: 0.5rem;
      border: none;
      font-size: 0.9rem;
      font-weight: 600;
      cursor: pointer;
      transition: all 0.15s ease;
    }}
    .btn-primary {{
      background: linear-gradient(180deg, #3b82f6, #2563eb);
      color: white;
    }}
    .btn-primary:hover {{
      background: linear-gradient(180deg, #60a5fa, #3b82f6);
    }}
    .btn-primary:disabled {{
      opacity: 0.6;
      cursor: not-allowed;
    }}
    .btn-danger {{
      background: linear-gradient(180deg, #ef4444, #dc2626);
      color: white;
    }}
    .btn-danger:hover {{
      background: linear-gradient(180deg, #f87171, #ef4444);
    }}
    .btn-secondary {{
      background: #374151;
      color: #e2e8f0;
    }}
    .btn-secondary:hover {{
      background: #4b5563;
    }}
    .status {{
      padding: 0.75rem 1rem;
      border-radius: 0.5rem;
      margin-top: 1rem;
      font-size: 0.9rem;
      display: none;
    }}
    .status.is-success {{
      display: block;
      background: rgba(34, 197, 94, 0.15);
      border: 1px solid #22c55e;
      color: #86efac;
    }}
    .status.is-error {{
      display: block;
      background: rgba(239, 68, 68, 0.15);
      border: 1px solid #ef4444;
      color: #fca5a5;
    }}
    .status.is-info {{
      display: block;
      background: rgba(59, 130, 246, 0.15);
      border: 1px solid #3b82f6;
      color: #93c5fd;
    }}
  </style>
</head>
<body>
  <main class="app">
    <h1>{title}</h1>
    <p class="subtitle">Edit the configuration file and save changes. Restart the service to apply.</p>

    <div class="destination-hash">
      <span class="label">Destination:</span>
      <span class="hash">{destination_hash}</span>
    </div>

    <div class="card">
      <div class="card-title">{config_name}</div>
      <div class="editor-wrap">
        <textarea id="config-editor" spellcheck="false">Loading...</textarea>
      </div>
      <div class="btn-row">
        <button id="save-btn" class="btn btn-primary" type="button">Save Config</button>
        <button id="reload-btn" class="btn btn-secondary" type="button">Reload</button>
        <button id="restart-btn" class="btn btn-danger" type="button">Restart Service</button>
      </div>
      <div id="status" class="status"></div>
    </div>
  </main>
  <script>
    (function() {{
      const editor = document.getElementById('config-editor');
      const saveBtn = document.getElementById('save-btn');
      const reloadBtn = document.getElementById('reload-btn');
      const restartBtn = document.getElementById('restart-btn');
      const statusEl = document.getElementById('status');

      function showStatus(message, type) {{
        statusEl.textContent = message;
        statusEl.className = 'status is-' + type;
      }}

      function clearStatus() {{
        statusEl.className = 'status';
      }}

      async function loadConfig() {{
        try {{
          const resp = await fetch('/api/config');
          if (!resp.ok) {{
            const data = await resp.json().catch(() => ({{}}));
            throw new Error(data.detail || 'HTTP ' + resp.status);
          }}
          const data = await resp.json();
          editor.value = data.config;
          clearStatus();
        }} catch (err) {{
          showStatus('Error loading config: ' + err.message, 'error');
        }}
      }}

      async function saveConfig() {{
        saveBtn.disabled = true;
        try {{
          const resp = await fetch('/api/config', {{
            method: 'PUT',
            headers: {{ 'Content-Type': 'application/json' }},
            body: JSON.stringify({{ config: editor.value }})
          }});
          const data = await resp.json().catch(() => ({{}}));
          if (!resp.ok) {{
            throw new Error(data.detail || 'HTTP ' + resp.status);
          }}
          showStatus(data.detail || 'Config saved successfully', 'success');
        }} catch (err) {{
          showStatus('Error saving config: ' + err.message, 'error');
        }} finally {{
          saveBtn.disabled = false;
        }}
      }}

      async function waitForService(maxAttempts = 30, interval = 500) {{
        for (let i = 0; i < maxAttempts; i++) {{
          await new Promise(r => setTimeout(r, interval));
          try {{
            const resp = await fetch('/api/config', {{ method: 'GET' }});
            if (resp.ok) {{
              return true;
            }}
          }} catch (e) {{
            // Service not yet available, continue polling
          }}
        }}
        return false;
      }}

      async function restartService() {{
        if (!confirm('Are you sure you want to restart the service?')) {{
          return;
        }}
        restartBtn.disabled = true;
        showStatus('Restarting service...', 'info');
        try {{
          // Send restart request - expect this to fail due to connection loss
          await fetch('/api/restart', {{ method: 'POST' }}).catch(() => {{}});

          // Wait a moment for the service to begin shutting down
          await new Promise(r => setTimeout(r, 1000));

          // Poll until the service comes back up
          showStatus('Waiting for service to restart...', 'info');
          const isUp = await waitForService();

          if (isUp) {{
            showStatus('Service restarted successfully', 'success');
            loadConfig();
          }} else {{
            showStatus('Service restart timed out - please refresh the page', 'error');
          }}
        }} catch (err) {{
          showStatus('Error restarting service: ' + err.message, 'error');
        }} finally {{
          restartBtn.disabled = false;
        }}
      }}

      saveBtn.addEventListener('click', saveConfig);
      reloadBtn.addEventListener('click', loadConfig);
      restartBtn.addEventListener('click', restartService);

      loadConfig();
    }})();
  </script>
</body>
</html>
"#, title = title, config_name = config_name, destination_hash = destination_hash))
}
