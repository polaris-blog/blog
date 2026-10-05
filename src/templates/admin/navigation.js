// Navigation editor — add, remove and reorder custom link rows.
// Rows are submitted in DOM order; the backend pairs label[]/url[] by index
// and drops rows left empty.
(function () {
  "use strict";

  var list = document.getElementById("nav-edit-list");
  var emptyHint = document.getElementById("nav-empty");
  var template = document.getElementById("nav-row-template");
  var addBtn = document.getElementById("nav-add-row");

  if (!list) return;

  function rows() {
    return Array.prototype.slice.call(list.querySelectorAll(".nav-edit-row"));
  }

  function refresh() {
    var all = rows();
    all.forEach(function (row, index) {
      var up = row.querySelector('[data-nav-move="up"]');
      var down = row.querySelector('[data-nav-move="down"]');
      if (up) up.disabled = index === 0;
      if (down) down.disabled = index === all.length - 1;
    });
    if (emptyHint) emptyHint.hidden = all.length > 0;
  }

  function moveRow(row, up) {
    var sibling = up ? row.previousElementSibling : row.nextElementSibling;
    if (!sibling) return;
    list.insertBefore(row, up ? sibling : sibling.nextElementSibling);
    refresh();
  }

  function bindRow(row) {
    row.querySelectorAll("[data-nav-move]").forEach(function (btn) {
      btn.addEventListener("click", function () {
        moveRow(row, btn.getAttribute("data-nav-move") === "up");
      });
    });
    var remove = row.querySelector("[data-nav-remove]");
    if (remove) {
      remove.addEventListener("click", function () {
        row.remove();
        refresh();
      });
    }
  }

  rows().forEach(bindRow);
  refresh();

  if (addBtn && template) {
    addBtn.addEventListener("click", function () {
      list.appendChild(template.content.cloneNode(true));
      var row = list.lastElementChild;
      if (!row) return;
      bindRow(row);
      refresh();
      var input = row.querySelector("input");
      if (input) input.focus();
    });
  }
})();
