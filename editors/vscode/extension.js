// Starts `calyx lsp` for .clyx files: diagnostics, outline, go to
// definition and hover come from the language server in the calyx binary.
const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");

let client;

function activate(context) {
  const command = vscode.workspace.getConfiguration("calyx").get("path") || "calyx";
  client = new LanguageClient(
    "calyx",
    "Calyx",
    { command, args: ["lsp"] },
    { documentSelector: [{ scheme: "file", language: "calyx" }] }
  );
  client.start().catch((e) => {
    vscode.window.showErrorMessage(
      `Calyx: could not start \`${command} lsp\` (${e.message}). Install calyx, or set calyx.path.`
    );
  });
  context.subscriptions.push({ dispose: () => client && client.stop() });
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
