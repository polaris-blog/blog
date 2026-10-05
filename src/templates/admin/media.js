// Polaris admin media layer — progressive enhancement only.
// Every operation works without it via plain form posts; this adds upload
// progress, drag-drop, clipboard paste, the batch toolbar and post-editor
// image insertion.
(() => {
  "use strict";

  const csrf = document.querySelector('meta[name="csrf"]')?.content || "";
  const folderMeta = document.querySelector('meta[name="media-folder"]');

  // ---------------------------------------------------------------------------
  // Upload plumbing (shared by library + editor)
  // ---------------------------------------------------------------------------

  const statusEl = (() => {
    const el = document.createElement("div");
    el.className = "media-upload-status";
    el.setAttribute("role", "status");
    el.setAttribute("aria-live", "polite");
    el.hidden = true;
    document.body.appendChild(el);
    return el;
  })();

  let activeUploads = 0;
  let uploadQueue = Promise.resolve();
  const uploadItems = [];
  let reloadRequested = false;

  function renderStatus(items) {
    if (!items.length) {
      statusEl.hidden = true;
      return;
    }
    statusEl.hidden = false;
    statusEl.replaceChildren(
      ...items.map((it) => {
        const row = document.createElement("div");
        row.className = "media-upload-row";
        const name = document.createElement("span");
        name.className = "media-upload-name";
        name.textContent = it.name;
        const bar = document.createElement("span");
        bar.className = "media-upload-bar";
        const fill = document.createElement("span");
        fill.className = "media-upload-fill";
        fill.style.width = (it.pct || 0) + "%";
        bar.appendChild(fill);
        row.append(name, bar);
        if (it.error) {
          const error = document.createElement("span");
          error.className = "media-upload-error";
          error.textContent = it.error;
          row.appendChild(error);
        }
        return row;
      })
    );
  }

  function uploadFile(file, folderId, onProgress) {
    return new Promise((resolve, reject) => {
      const form = new FormData();
      form.append("csrf", csrf);
      if (folderId) form.append("folder_id", folderId);
      form.append("file", file, file.name);
      const xhr = new XMLHttpRequest();
      xhr.open("POST", "/api/media/upload");
      xhr.responseType = "json";
      xhr.timeout = 300000;
      xhr.upload.addEventListener("progress", (e) => {
        if (e.lengthComputable) onProgress(Math.round((e.loaded / e.total) * 100));
      });
      xhr.addEventListener("load", () => {
        if (xhr.status >= 200 && xhr.status < 300 && xhr.response?.data?.length) {
          resolve(xhr.response.data[0]);
        } else {
          const msg =
            xhr.response?.rejected?.[0]?.error ||
            xhr.response?.error?.message ||
            (typeof xhr.response === "string" ? xhr.response : "") ||
            "upload failed (" + xhr.status + ")";
          reject(new Error(msg));
        }
      });
      xhr.addEventListener("error", () => reject(new Error("network error")));
      xhr.addEventListener("timeout", () => reject(new Error("Upload timed out. Check the media library before retrying.")));
      xhr.addEventListener("abort", () => reject(new Error("Upload cancelled.")));
      xhr.send(form);
    });
  }

  async function uploadMany(files, { folderId, reload }) {
    const list = Array.from(files);
    if (!list.length) return;
    const items = list.map((f) => ({ name: f.name, pct: 0 }));
    const failures = [];
    uploadItems.push(...items);
    renderStatus(uploadItems);
    reloadRequested ||= reload;
    activeUploads++;
    const previous = uploadQueue;
    let release;
    uploadQueue = new Promise((resolve) => { release = resolve; });
    await previous;
    for (let i = 0; i < list.length; i++) {
      try {
        await uploadFile(list[i], folderId, (pct) => {
          items[i].pct = pct;
          renderStatus(uploadItems);
        });
        items[i].pct = 100;
        items[i].done = true;
      } catch (err) {
        failures.push(list[i].name + ": " + err.message);
        items[i].pct = 0;
        items[i].error = err.message;
      }
      renderStatus(uploadItems);
    }
    activeUploads--;
    release();
    if (!activeUploads && !uploadItems.some((item) => item.error)) {
      setTimeout(() => {
        if (activeUploads || uploadItems.some((item) => item.error)) return;
        if (reloadRequested) location.reload();
        else { uploadItems.length = 0; renderStatus([]); }
      }, reloadRequested ? 350 : 4000);
    }
    return failures;
  }

  // ---------------------------------------------------------------------------
  // Library page: picker, drag-drop, paste, batch toolbar
  // ---------------------------------------------------------------------------

  const library = document.querySelector("[data-media-library]");
  if (library) {
    const folderId = folderMeta && /^\d+$/.test(folderMeta.content) ? folderMeta.content : "";
    const fileInput = document.getElementById("media-file-input");
    if (fileInput) {
      fileInput.addEventListener("change", () => {
        uploadMany(fileInput.files, { folderId, reload: true });
        fileInput.value = "";
      });
    }

    let dragDepth = 0;
    document.addEventListener("dragenter", (e) => {
      if (e.dataTransfer?.types?.includes("Files")) {
        dragDepth++;
        document.body.classList.add("media-dragging");
      }
    });
    document.addEventListener("dragover", (e) => {
      if (e.dataTransfer?.types?.includes("Files")) e.preventDefault();
    });
    document.addEventListener("dragleave", () => {
      dragDepth = Math.max(0, dragDepth - 1);
      if (!dragDepth) document.body.classList.remove("media-dragging");
    });
    document.addEventListener("drop", (e) => {
      if (!e.dataTransfer?.types?.includes("Files")) return;
      e.preventDefault();
      dragDepth = 0;
      document.body.classList.remove("media-dragging");
      uploadMany(e.dataTransfer.files, { folderId, reload: true });
    });
    document.addEventListener("paste", (e) => {
      const files = Array.from(e.clipboardData?.files || []).filter((f) => f.type);
      if (files.length) {
        e.preventDefault();
        uploadMany(files, { folderId, reload: true });
      }
    });

    // Batch toolbar
    const batchForm = document.querySelector(".media-batch");
    if (batchForm) {
      const bar = batchForm.querySelector("[data-batch-bar]");
      const boxes = () => Array.from(batchForm.querySelectorAll('input[name="ids"]'));
      const count = bar.querySelector(".batch-count");
      const actionSel = bar.querySelector('select[name="action"]');
      const folderSel = bar.querySelector("[data-batch-folder]");
      const tagsInput = bar.querySelector("[data-batch-tags]");
      const forceBox = bar.querySelector("[data-batch-force]");

      function refresh() {
        const n = boxes().filter((b) => b.checked).length;
        bar.hidden = n === 0;
        count.textContent = n + " selected";
        const action = actionSel.value;
        folderSel.disabled = action !== "move";
        tagsInput.disabled = action !== "tag";
        if (forceBox) {
          forceBox.hidden = action !== "delete";
          if (action !== "delete") forceBox.querySelector("input").checked = false;
        }
      }

      const allBox = bar.querySelector("[data-select-all]");
      allBox?.addEventListener("change", () => {
        boxes().forEach((b) => (b.checked = allBox.checked));
        refresh();
      });
      batchForm.addEventListener("change", (e) => {
        if (e.target.matches('input[name="ids"]')) refresh();
      });
      actionSel.addEventListener("change", refresh);
      refresh();

      batchForm.addEventListener("submit", (e) => {
        if (actionSel.value === "delete" && !confirm("Delete the selected media?")) {
          e.preventDefault();
        }
      });
    }
  }

  // ---------------------------------------------------------------------------
  // Post/page editor: paste + drag-drop images → Markdown insert
  // ---------------------------------------------------------------------------

  const editor = document.querySelector(".content-editor");
  if (editor && csrf) {
    const folderId = "";
    const insert = (text) => {
      const start = editor.selectionStart ?? editor.value.length;
      const end = editor.selectionEnd ?? start;
      const before = editor.value.slice(0, start);
      const after = editor.value.slice(end);
      const pad = before && !before.endsWith("\n") ? "\n" : "";
      editor.value = before + pad + text + "\n" + after;
      const pos = (before + pad + text).length + 1;
      editor.selectionStart = editor.selectionEnd = pos;
      editor.dispatchEvent(new Event("input", { bubbles: true }));
    };

    editor.addEventListener("paste", (e) => {
      const files = Array.from(e.clipboardData?.files || []).filter((f) => f.type.startsWith("image/"));
      if (!files.length) return;
      e.preventDefault();
      (async () => {
        for (const file of files) {
          try {
            const m = await uploadFile(file, folderId, () => {});
            insert("![" + (m.filename || "image") + "](" + (m.url || m.path) + ")");
          } catch (err) {
            alert(file.name + ": " + err.message);
          }
        }
      })();
    });

    let over = false;
    editor.addEventListener("dragover", (e) => {
      if (e.dataTransfer?.types?.includes("Files")) {
        e.preventDefault();
        if (!over) {
          over = true;
          editor.classList.add("media-editor-over");
        }
      }
    });
    editor.addEventListener("dragleave", () => {
      over = false;
      editor.classList.remove("media-editor-over");
    });
    editor.addEventListener("drop", (e) => {
      const files = Array.from(e.dataTransfer?.files || []).filter((f) => f.type.startsWith("image/"));
      if (!files.length) return;
      e.preventDefault();
      over = false;
      editor.classList.remove("media-editor-over");
      (async () => {
        for (const file of files) {
          try {
            const m = await uploadFile(file, folderId, () => {});
            insert("![" + (m.filename || "image") + "](" + (m.url || m.path) + ")");
          } catch (err) {
            alert(file.name + ": " + err.message);
          }
        }
      })();
    });
  }

  // ---------------------------------------------------------------------------
  // Detail page: copy URL buttons
  // ---------------------------------------------------------------------------

  const coverInput = document.getElementById("f-featured");
  const coverControls = document.querySelector("[data-cover-controls]");
  if (coverInput && coverControls && csrf) {
    const picker = document.getElementById("cover-upload");
    const preview = coverControls.querySelector("[data-cover-preview]");
    const status = coverControls.querySelector("[data-cover-status]");
    const clear = coverControls.querySelector("[data-cover-clear]");
    let uploading = false;
    coverControls.hidden = false;

    function refreshCover() {
      const value = coverInput.value.trim();
      let valid = false;
      try {
        const url = new URL(value, location.origin);
        valid = !!value && ["http:", "https:"].includes(url.protocol);
      } catch (_) {}
      preview.hidden = !valid;
      if (valid) preview.src = value;
      else preview.removeAttribute("src");
    }
    preview.addEventListener("error", () => { preview.hidden = true; });
    coverInput.addEventListener("input", refreshCover);
    clear.addEventListener("click", () => {
      coverInput.value = "";
      coverInput.dispatchEvent(new Event("input", { bubbles: true }));
      status.textContent = "Cover removed. Save the post to apply.";
    });
    coverInput.form.addEventListener("submit", (e) => {
      if (uploading) {
        e.preventDefault();
        status.textContent = "Please wait for the cover upload to finish.";
      }
    });
    picker.addEventListener("change", async () => {
      const file = picker.files[0];
      if (!file) return;
      uploading = true;
      picker.disabled = clear.disabled = true;
      coverInput.readOnly = true;
      status.textContent = "Uploading cover…";
      try {
        const media = await uploadFile(file, "", (pct) => {
          status.textContent = "Uploading cover… " + pct + "%";
        });
        coverInput.value = media.url || media.path;
        coverInput.dispatchEvent(new Event("input", { bubbles: true }));
        status.textContent = "Cover uploaded. Save the post to apply.";
      } catch (err) {
        status.textContent = err.message;
      } finally {
        uploading = false;
        picker.disabled = clear.disabled = false;
        coverInput.readOnly = false;
        picker.value = "";
      }
    });
    refreshCover();
  }

  document.querySelectorAll("[data-copy-btn]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const input = btn.parentElement.querySelector("[data-copy-target]");
      try {
        await navigator.clipboard.writeText(input.value);
        btn.textContent = "Copied";
        setTimeout(() => (btn.textContent = "Copy"), 1500);
      } catch {
        input.select();
        document.execCommand("copy");
      }
    });
  });
})();
