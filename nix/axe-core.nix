# axe-core (MPL-2.0) at a pinned release, for the accessibility checks in the browser
# tests. Test-only: e2e injects it through WebDriver; the server never serves it.
{ fetchzip }:
fetchzip {
  url = "https://registry.npmjs.org/axe-core/-/axe-core-4.14.1.tgz";
  hash = "sha256-tQAUrNS7wjTaawUcHiSxFmRl7GkJ9vr2740iiTtdIBk=";
}
