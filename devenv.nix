{ pkgs, lib, config, inputs, ... }:

{
  # https://devenv.sh/basics/
  env.DATABASE_URL = "./test.db";

  dotenv.enable = true;

  # https://devenv.sh/packages/
  packages = with pkgs ; [
    git
    git
    sqlite-web
    sqlite
    diesel-cli
    sqlx-cli
    llvmPackages_12.stdenv
    llvmPackages_12.clang-unwrapped

    lldb
  ];

  # https://devenv.sh/languages/
  # languages.rust.enable = true;

  # https://devenv.sh/processes/
  # processes.cargo-watch.exec = "cargo-watch";

  # https://devenv.sh/services/
  # services.postgres.enable = true;

  # https://devenv.sh/scripts/
  scripts.hello.exec = ''
    echo hello from $GREET
  '';

  enterShell = ''
    hello
    git --version
  '';

  # https://devenv.sh/tasks/
  # tasks = {
  #   "myproj:setup".exec = "mytool build";
  #   "devenv:enterShell".after = [ "myproj:setup" ];
  # };

  # https://devenv.sh/tests/
  enterTest = ''
    echo "Running tests"
    git --version | grep --color=auto "${pkgs.git.version}"
  '';

  # https://devenv.sh/pre-commit-hooks/
  # pre-commit.hooks.shellcheck.enable = true;

  # See full reference at https://devenv.sh/reference/options/
  languages.rust = {
    enable = true;
    channel = "nightly";
    components = [
      "rustc" "cargo" "clippy" "rustfmt" "rust-analyzer"
      "miri"
    ];
  };
}
