import { onHostEvent } from "./bridge";

/**
 * The mobile game screen as the native view reports it (AND-APP-16), in CSS
 * pixels of this page. Controls may sit in the letterbox around `game`,
 * including the safe-area margins, but never on a cutout, within
 * `cornerRadius` of a corner or on a `gestures` edge.
 */
export interface Area {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface GameLayout {
  width: number;
  height: number;
  /** The presented game frame. */
  game: Area;
  /** Notches, Dynamic Islands and punch holes. */
  cutouts: Area[];
  cornerRadius: number;
  /** Edges whose swipes the system takes, such as the home indicator. */
  gestures: { top: number; right: number; bottom: number; left: number };
}

/** The latest layout; null until a game view reports one. */
export const screen = $state<{ layout: GameLayout | null }>({ layout: null });

/** Follows the native layout while the game screen is shown. */
export function trackScreen(): () => void {
  const stop = onHostEvent<GameLayout>("game_layout", (layout) => {
    screen.layout = layout;
  });
  return () => {
    stop();
    screen.layout = null;
  };
}
