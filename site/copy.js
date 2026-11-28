// Adds a Copy button to each command block. Without JavaScript the blocks
// are still plain text to select, and there's no button that does nothing.
// How long "Copied" shows: the app's own notice time, from tokens.css.
const noticeMs = parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--notice-time")) || 0;

for (const block of document.querySelectorAll(".code")) {
  const code = block.querySelector("code");
  const button = document.createElement("button");
  button.type = "button";
  button.className = "copy";
  // The accessible name starts with the visible text, plus what it copies.
  const what = block.dataset.copyWhat || "";
  const show = (text, withWhat) => {
    button.textContent = text;
    button.setAttribute("aria-label", withWhat && what ? `${text} ${what}` : text);
  };
  show("Copy", true);
  let reset;
  const status = document.createElement("span");
  status.setAttribute("role", "status");
  status.className = "visually-hidden";
  button.addEventListener("click", async () => {
    clearTimeout(reset);
    // Cleared first, so the same message is announced again.
    status.textContent = "";
    // Copy the commands only: no prompts, no comments.
    const text = [...code.childNodes]
      .filter((n) => !(n.classList && (n.classList.contains("prompt") || n.classList.contains("comment"))))
      .map((n) => n.textContent)
      .join("")
      .split("\n")
      .map((line) => line.trimEnd())
      .join("\n");
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Stays until the next click, so there's time to read it.
      show("Select the text to copy", false);
      status.textContent = "Couldn't copy. Select the text to copy it.";
      return;
    }
    show("Copied", true);
    status.textContent = "Copied to the clipboard";
    reset = setTimeout(() => {
      show("Copy", true);
      status.textContent = "";
    }, noticeMs);
  });
  block.classList.add("copyable");
  block.append(button, status);
}
