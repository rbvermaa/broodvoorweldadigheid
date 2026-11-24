"use strict";

// Pages with forms render <meta name="csrf-token">; HTMX requests send it as a header.
document.addEventListener("htmx:configRequest", (event) => {
  const token = document.querySelector('meta[name="csrf-token"]');
  if (token) {
    event.detail.headers["X-CSRF-Token"] = token.content;
  }
});

// Confirmation dialogs (templates/components/dialog.html).
document.addEventListener("click", (event) => {
  const opener = event.target.closest("[data-dialog-open]");
  if (opener) {
    document.getElementById(opener.dataset.dialogOpen)?.showModal();
    return;
  }
  const closer = event.target.closest("[data-dialog-close]");
  if (closer) {
    closer.closest("dialog")?.close();
  }
});

document.addEventListener("htmx:afterRequest", (event) => {
  event.target.closest("dialog")?.close();
});
