mermaid.initialize({
  theme: "neutral",
  securityLevel: "strict",
});

document.addEventListener("DOMContentLoaded", function () {
  mermaid.run({ querySelector: ".mermaid" });
});

// The script runs after DOMContentLoaded in MkDocs, so run immediately too.
if (document.readyState !== "loading") {
  mermaid.run({ querySelector: ".mermaid" });
}
