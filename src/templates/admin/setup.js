/* Admin setup wizard: show only the fields relevant to the selected
   database / cache driver. The server renders the initial visibility
   (server-selected driver carries the `visible` class), this script only
   keeps the groups in sync when the selects change. CSP-safe external file. */
(function () {
  "use strict";
  var driver = document.getElementById("db-driver");
  var cache = document.getElementById("cache-driver");
  function sync() {
    var dbValue = driver ? driver.value : "";
    var cacheValue = cache ? cache.value : "";
    document.querySelectorAll("[data-fields]").forEach(function (el) {
      var targets = (el.getAttribute("data-fields") || "").split(" ");
      var visible;
      if (targets[0] === "redis") {
        visible = cacheValue === "redis";
      } else {
        visible = targets.indexOf(dbValue) !== -1;
      }
      el.classList.toggle("visible", visible);
    });
  }
  if (driver) driver.addEventListener("change", sync);
  if (cache) cache.addEventListener("change", sync);
})();
