# shell.nix
with import <nixpkgs> {};

let
  python-with-packages = python3.withPackages (ps: [
    ps.feedparser
    ps.beautifulsoup4
    ps.lxml
    ps.pytz
    ps.flask
  ]);
in
  mkShell {
    buildInputs = [
      python-with-packages
    ];
    shellHook = ''
      echo "--- Greg's feed shell ---"
      echo "Flask environment is ready."
      echo "Run 'python app.py' to start the server."
    '';
  }