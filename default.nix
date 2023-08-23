# save this as shell.nix
{ pkgs ? import <nixpkgs> {}}:

pkgs.mkShell {
  packages = [ 
    pkgs.rustc
    pkgs.cargo
    pkgs.rust-analyzer
  ];
}
