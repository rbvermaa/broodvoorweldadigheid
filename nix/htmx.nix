# htmx (0BSD) at a pinned release. apps/server/src/assets.rs embeds it into the server binary,
# so nothing third-party is served from a CDN and no minified JS lives in the repository.
{ fetchurl }:
fetchurl {
  url = "https://github.com/bigskysoftware/htmx/releases/download/v2.0.11/htmx.min.js";
  hash = "sha256-1v3HXyBOa976mbab8ebUrGm4o2T3eSn0XBNHa0AA9xc=";
}
