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

// A failed request leaves the dialog open, so the action does not look done.
document.addEventListener("htmx:afterRequest", (event) => {
  if (event.detail.successful) {
    event.target.closest("dialog")?.close();
  }
});

// Escape puts a recipe cell back to its saved value, so leaving it saves nothing
// (templates/components/recipe.html, method.html).
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && event.target.matches(".recipe-input, .method-table .field-input")) {
    event.target.value = event.target.defaultValue;
  }
});
