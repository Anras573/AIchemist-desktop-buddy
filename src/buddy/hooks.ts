// Extension points on the cat for later tasks. The renderer (main.ts) already
// reports the interaction to Rust and suppresses the browser context menu;
// these handlers add the UI on top.
//
// - 2.3 (main.ts): `onCatClick` toggles the upcoming popover,
//   `onCatContextMenu` shows the cat menu (`show_cat_menu`).

export type CatHandler = (e: MouseEvent) => void;

const noop: CatHandler = () => {};

export const catHandlers = {
  click: noop,
  contextMenu: noop,
};

/** Left-click on the cat. */
export function onCatClick(handler: CatHandler): void {
  catHandlers.click = handler;
}

/** Right-click on the cat (the browser menu is already prevented). */
export function onCatContextMenu(handler: CatHandler): void {
  catHandlers.contextMenu = handler;
}
