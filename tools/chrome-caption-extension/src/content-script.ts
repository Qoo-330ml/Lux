const PAGE_SOURCE = "lux-caption-page";
const EXTENSION_SOURCE = "lux-caption-extension";
const MEDIA_PAGE_SOURCE = "lux-media-page";
const MEDIA_EXTENSION_SOURCE = "lux-media-extension";
const PROTOCOL_VERSION = 1;

// A DOM marker is the only capability probe exposed to the Lux page. The
// page never receives an extension API object or a remote media URL from this
// content script; it only knows that the optional bridge is installed.
document.documentElement?.setAttribute("data-lux-media-extension", "1");

window.addEventListener("message", (event: MessageEvent<Record<string, unknown>>) => {
  if (event.source !== window || event.origin !== window.location.origin) return;
  const message = event.data;
  if (!message || message.version !== PROTOCOL_VERSION
    || (message.source !== PAGE_SOURCE && message.source !== MEDIA_PAGE_SOURCE)) return;
  chrome.runtime.sendMessage(message, (response) => {
    if (chrome.runtime.lastError || !response || typeof response !== "object") return;
    postResponse(response as Record<string, unknown>);
  });
});

chrome.runtime.onMessage.addListener((message) => {
  if (!message || typeof message !== "object") return;
  const event = message as Record<string, unknown>;
  if (event.source !== EXTENSION_SOURCE || event.version !== PROTOCOL_VERSION) return;
  postResponse(event);
});

function postResponse(message: Record<string, unknown>) {
  if (typeof message.sessionId !== "string"
    || (message.source !== EXTENSION_SOURCE && message.source !== MEDIA_EXTENSION_SOURCE)) return;
  window.postMessage(message, window.location.origin);
}
