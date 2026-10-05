// Polaris admin extension upload — progressive enhancement only.
// The plain form post works without JS; this adds drag-drop, upload
// progress and inline error reporting.
(() => {
  "use strict";

  for (const panel of document.querySelectorAll("[data-ext-upload]")) {
    const form = panel.querySelector("form");
    const drop = panel.querySelector("[data-drop-zone]");
    const input = panel.querySelector("[data-package-input]");
    const btn = panel.querySelector("[data-upload-btn]");
    const fileName = panel.querySelector("[data-file-name]");
    const progress = panel.querySelector("[data-progress]");
    const fill = progress?.querySelector(".ext-progress-fill");
    const error = panel.querySelector("[data-error]");
    if (!form || !drop || !input || !btn) continue;
    let uploading = false;
    if (error) error.setAttribute("role", "alert");

    const setError = (msg) => {
      if (!error) return;
      error.hidden = !msg;
      error.textContent = msg || "";
    };

    const showFile = (file) => {
      if (uploading) return;
      setError("");
      if (fileName) {
        fileName.hidden = false;
        fileName.textContent = file ? `${file.name} (${Math.ceil(file.size / 1024)} KB)` : "";
      }
      btn.disabled = !file;
      if (file && !/\.zip$/i.test(file.name)) {
        setError("Only .zip packages are supported.");
        btn.disabled = true;
      }
    };

    input.addEventListener("change", () => showFile(input.files[0]));

    // Clicking anywhere in the zone opens the picker (except the label,
    // which already does).
    drop.addEventListener("click", (e) => {
      if (uploading || e.target === input || e.target.closest("label")) return;
      input.click();
    });

    drop.addEventListener("dragover", (e) => {
      e.preventDefault();
      drop.classList.add("ext-drop-active");
    });
    drop.addEventListener("dragleave", () => drop.classList.remove("ext-drop-active"));
    drop.addEventListener("drop", (e) => {
      e.preventDefault();
      drop.classList.remove("ext-drop-active");
      const file = e.dataTransfer?.files?.[0];
      if (!file || uploading) return;
      const dt = new DataTransfer();
      dt.items.add(file);
      input.files = dt.files;
      showFile(file);
    });
    drop.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        input.click();
      }
    });

    form.addEventListener("submit", (e) => {
      e.preventDefault();
      if (uploading) return;
      if (!input.files.length) {
        e.preventDefault();
        setError("Choose a .zip package first.");
        return;
      }
      e.preventDefault();
      uploading = true;
      form.setAttribute("aria-busy", "true");
      setError("");
      btn.disabled = true;
      btn.textContent = "Uploading…";
      if (progress) progress.hidden = false;
      if (fill) fill.style.width = "0%";

      const fd = new FormData(form);
      const xhr = new XMLHttpRequest();
      xhr.open("POST", "/api/admin/extensions/upload");
      xhr.responseType = "json";
      xhr.timeout = 300000;
      xhr.upload.addEventListener("progress", (ev) => {
        if (ev.lengthComputable && fill) {
          fill.style.width = Math.round((ev.loaded / ev.total) * 100) + "%";
        }
      });
      xhr.addEventListener("load", () => {
        if (xhr.status >= 200 && xhr.status < 300 && xhr.response?.installed) {
          if (fill) fill.style.width = "100%";
          const it = xhr.response.installed;
          const perm = it.permissions?.length
            ? ` — permissions: ${it.permissions.join(", ")}`
            : "";
          if (fileName) {
            fileName.hidden = false;
            fileName.textContent = `${it.name} v${it.version} ${it.action} (SHA-256 ${it.sha256.slice(0, 12)}…)${perm}`;
          }
          setTimeout(() => location.reload(), 1200);
          return;
        }
        const msg =
          xhr.response?.error?.message ||
          (typeof xhr.response === "string" ? xhr.response : "") ||
          `upload failed (${xhr.status})`;
        setError(msg);
        uploading = false;
        form.removeAttribute("aria-busy");
        btn.disabled = false;
        btn.textContent = "Upload";
        if (progress) progress.hidden = true;
      });
      const failed = (message) => {
        setError(message);
        uploading = false;
        form.removeAttribute("aria-busy");
        btn.disabled = false;
        btn.textContent = "Upload";
        if (progress) progress.hidden = true;
      };
      xhr.addEventListener("error", () => failed("Network error. Check the extension list before retrying."));
      xhr.addEventListener("timeout", () => failed("Upload timed out. Check the extension list before retrying."));
      xhr.addEventListener("abort", () => failed("Upload cancelled."));
      xhr.send(fd);
    });
  }
})();
