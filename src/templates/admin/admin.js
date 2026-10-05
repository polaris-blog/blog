// Shared progressive enhancement for the server-rendered admin.
(() => {
  "use strict";

  document.querySelectorAll("main table").forEach((table) => {
    let region = table.parentElement;
    if (!region.matches(".jobs-table, .table-scroll")) {
      region = document.createElement("div");
      region.className = "table-scroll";
      table.before(region);
      region.append(table);
    }
    region.tabIndex = 0;
    region.setAttribute("role", "region");
    region.setAttribute("aria-label", document.title.split(" · ")[0] + " table");
    table.querySelectorAll("thead th").forEach((th) => th.setAttribute("scope", "col"));
  });
  document.querySelectorAll(".menu a.active").forEach((a) => a.setAttribute("aria-current", "page"));

  // Bubble after individual handlers so AJAX and validation keep control.
  document.addEventListener("submit", (event) => {
    const form = event.target;
    if (event.defaultPrevented || !(form instanceof HTMLFormElement) || form.method !== "post") return;
    if (form.dataset.submitting) {
      event.preventDefault();
      return;
    }
    const path = new URL(form.action, location.href).pathname;
    const message = form.dataset.confirm ||
      (/\/delete$/.test(path) && !path.startsWith("/admin/jobs/")
        ? "Delete this item? This action cannot be undone." : "");
    if (message && !window.confirm(message)) {
      event.preventDefault();
      return;
    }
    form.dataset.submitting = "true";
    form.setAttribute("aria-busy", "true");
    // Keep named submitters in the native form payload.
    form.querySelectorAll('button[type="submit"], button:not([type]), input[type="submit"]')
      .forEach((button) => button.setAttribute("aria-disabled", "true"));
  });

  window.addEventListener("pageshow", () => {
    document.querySelectorAll("form[data-submitting]").forEach((form) => {
      delete form.dataset.submitting;
      form.removeAttribute("aria-busy");
      form.querySelectorAll('[aria-disabled="true"]').forEach((button) => button.removeAttribute("aria-disabled"));
    });
  });
})();
