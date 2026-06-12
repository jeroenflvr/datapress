// Click-to-zoom for Mermaid diagrams.
// Clicking a rendered diagram opens it in a fullscreen overlay; close with the
// X button, by clicking the backdrop, or by pressing ESC.
//
// Material for MkDocs renders each Mermaid diagram into a CLOSED shadow root on
// the <div class="mermaid"> host (attachShadow({mode:"closed"})). That means the
// inner <svg> is unreachable from the outside — querySelector and even
// host.shadowRoot return nothing. So instead of cloning the svg, we move the
// whole host element (which carries its closed shadow root and rendered svg)
// into the overlay and scale it up with a CSS transform, then move it back on
// close.

(function () {
  "use strict";

  var overlay, stage, savedState;

  function buildOverlay() {
    overlay = document.createElement("div");
    overlay.className = "dp-mermaid-overlay";
    overlay.setAttribute("role", "dialog");
    overlay.setAttribute("aria-modal", "true");
    overlay.hidden = true;

    var closeBtn = document.createElement("button");
    closeBtn.type = "button";
    closeBtn.className = "dp-mermaid-overlay__close";
    closeBtn.setAttribute("aria-label", "Close diagram");
    closeBtn.textContent = "\u00D7"; // ×

    stage = document.createElement("div");
    stage.className = "dp-mermaid-overlay__stage";

    overlay.appendChild(closeBtn);
    overlay.appendChild(stage);
    document.body.appendChild(overlay);

    closeBtn.addEventListener("click", close);
    overlay.addEventListener("click", function (e) {
      // Close when clicking the backdrop or the stage padding (not the diagram).
      if (e.target === overlay || e.target === stage) close();
    });
  }

  function open(host) {
    if (!overlay) buildOverlay();
    if (savedState) return; // already showing one

    var rect = host.getBoundingClientRect();
    var w = rect.width || host.offsetWidth || 320;
    var h = rect.height || host.offsetHeight || 200;

    // Remember where the host lived so we can put it back exactly.
    savedState = {
      host: host,
      parent: host.parentNode,
      next: host.nextSibling,
      cssText: host.style.cssText,
    };

    // Pin the host to its rendered size so layout (and the transform basis) stays
    // identical after we move it into the centered stage.
    host.style.width = w + "px";
    host.style.maxWidth = "none";
    host.style.margin = "0";

    stage.appendChild(host);

    // Scale to fill ~90% of the viewport (zoom in for small diagrams, fit for
    // large ones), capped so it never turns into a blurry mess.
    var availW = window.innerWidth * 0.92;
    var availH = window.innerHeight * 0.85;
    var scale = Math.min(availW / w, availH / h, 4);
    if (!isFinite(scale) || scale <= 0) scale = 1;
    host.style.transformOrigin = "center center";
    host.style.transform = "scale(" + scale + ")";

    overlay.hidden = false;
    document.body.classList.add("dp-mermaid-no-scroll");
  }

  function close() {
    if (!overlay || !savedState) return;
    var s = savedState;
    savedState = null;

    overlay.hidden = true;
    document.body.classList.remove("dp-mermaid-no-scroll");

    // Restore inline styles and original DOM position.
    s.host.style.cssText = s.cssText;
    if (s.parent) s.parent.insertBefore(s.host, s.next);
  }

  function isOpen() {
    return overlay && !overlay.hidden;
  }

  // A rendered Mermaid host is the <div class="mermaid"> (the pre-render source
  // is <pre class="mermaid"><code>…). Only treat rendered hosts as zoomable.
  function isRenderedHost(el) {
    return (
      el &&
      el.classList &&
      el.classList.contains("mermaid") &&
      el.tagName === "DIV"
    );
  }

  // Click delegation (capture phase). Clicks inside the closed shadow root are
  // retargeted to the .mermaid host, so e.target is the host (or an ancestor).
  document.addEventListener(
    "click",
    function (e) {
      if (isOpen()) return;
      var t = e.target;
      if (!t || !t.closest) return;
      var host = t.closest("div.mermaid");
      if (!isRenderedHost(host)) return;
      open(host);
    },
    true
  );

  document.addEventListener("keydown", function (e) {
    if (e.key === "Escape" && isOpen()) close();
  });

  // Give rendered diagrams a zoom-in cursor. Polls briefly (Mermaid renders
  // asynchronously) and re-runs on Material's instant-navigation page changes.
  function markZoomable() {
    document.querySelectorAll("div.mermaid").forEach(function (d) {
      d.classList.add("dp-mermaid-zoomable");
    });
  }

  function start() {
    markZoomable();
    var tries = 0;
    var timer = setInterval(function () {
      markZoomable();
      if (++tries > 40) clearInterval(timer);
    }, 250);
  }

  if (window.document$ && typeof window.document$.subscribe === "function") {
    window.document$.subscribe(start);
  } else if (document.readyState !== "loading") {
    start();
  } else {
    document.addEventListener("DOMContentLoaded", start);
  }
})();
