# shell.nix
with import <nixpkgs> {};

mkShell {
  buildInputs = [
    cargo
    nodejs
    rustc
  ];

  shellHook = ''
    echo "--- Greg's feed desktop shell ---"
    echo "Run 'npm install' once, then 'npm run dev' or 'npm run build'."
  '';
}
