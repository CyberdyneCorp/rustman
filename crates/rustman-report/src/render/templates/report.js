(function () {
  var severity = document.getElementById("filter-severity");
  var operation = document.getElementById("filter-operation");
  if (!severity || !operation) { return; }
  document.getElementById("filters").hidden = false;
  function apply() {
    var items = document.querySelectorAll("[data-severity]");
    for (var i = 0; i < items.length; i++) {
      var item = items[i];
      var show = (!severity.value || item.getAttribute("data-severity") === severity.value) &&
        (!operation.value || item.getAttribute("data-operation") === operation.value);
      item.hidden = !show;
    }
  }
  severity.addEventListener("change", apply);
  operation.addEventListener("change", apply);
})();
