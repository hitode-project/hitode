/** Why a library entry cannot be launched right now. */
export type Offline = "disk" | "missing";

export interface Game {
  id: string;
  title: string;
  brand: string;
  year: number;
  /** Placeholder cover class from covers.css until real cover art is wired. */
  cover: string;
  engine: string;
  /** Guest ANSI code page; 932 is Japanese. */
  codepage: number;
  /** Folder of the game as mounted; shown when it is offline. */
  path: string;
  /** Main program, relative to `path`. */
  exe: string;
  /** Unix milliseconds of the last session end; null if never started. */
  lastPlayed: number | null;
  playMinutes: number;
  favorite: boolean;
  nsfw: boolean;
  offline: Offline | null;
  /** Minutes played on each of the last 30 days, oldest first. */
  daily: number[];
  sessions30: number;
}

export type LibraryFilter = "all" | "recent" | "favorite" | "unstarted";
