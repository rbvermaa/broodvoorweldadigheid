"use strict";

// Pages with forms render <meta name="csrf-token">; HTMX requests send it as a header.
document.addEventListener("htmx:configRequest", (event) => {
  const token = document.querySelector('meta[name="csrf-token"]');
  if (token) {
    event.detail.headers["X-CSRF-Token"] = token.content;
  }
});
