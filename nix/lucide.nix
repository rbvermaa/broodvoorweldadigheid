# Lucide icons (ISC) from the lucide-static npm release. apps/server/build.rs copies only
# the icons listed in design/icons.toml into the server binary.
{ fetchzip }:
fetchzip {
  url = "https://registry.npmjs.org/lucide-static/-/lucide-static-1.66.0.tgz";
  hash = "sha256-zmonqyKgtp/pqZaXkBK5PEh2gnd12PKVYV6COgx/MNg=";
}
