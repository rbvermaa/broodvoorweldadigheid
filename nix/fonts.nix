# Oswald (headings) and Inter (body), both SIL OFL 1.1, as variable woff2 files.
# apps/server/src/assets.rs embeds them into the server binary.
{
  runCommand,
  fetchurl,
  inter,
  woff2,
}:
let
  # The google/fonts commit that nixpkgs' google-fonts package pins.
  googleFonts = "https://raw.githubusercontent.com/google/fonts/5174b3333331c966c38f4355d50b03ca1c1df2f9/ofl/oswald";
  oswald = fetchurl {
    name = "Oswald-wght.ttf";
    url = "${googleFonts}/Oswald%5Bwght%5D.ttf";
    hash = "sha256-WzjCRuJVoS9XEtZA1WvM7QRyRm/GiYPS0EEOwEV8KBc=";
  };
  oswaldLicence = fetchurl {
    url = "${googleFonts}/OFL.txt";
    hash = "sha256-D9cxqQS3KaTgLq9ejr0GeD7dmr5ADoiCdgFgIwZ1tlI=";
  };
in
runCommand "bvw-fonts"
  {
    nativeBuildInputs = [ woff2 ];
  }
  ''
    mkdir -p $out work
    cp ${inter.src}/web/InterVariable.woff2 $out/inter.woff2
    cp ${inter.src}/LICENSE.txt $out/inter-OFL.txt
    cp ${oswald} work/oswald.ttf
    woff2_compress work/oswald.ttf
    cp work/oswald.woff2 $out/oswald.woff2
    cp ${oswaldLicence} $out/oswald-OFL.txt
  ''
