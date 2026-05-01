<script>
(function () {
  if (!window.EventSource) return;
  var es = new EventSource("/__brief_web_events");
  es.addEventListener("reload", function () { window.location.reload(); });
  es.onerror = function () { /* keep retrying silently */ };
}());
</script>
