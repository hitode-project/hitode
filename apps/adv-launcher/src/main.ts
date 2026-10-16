import { mount } from "svelte";
import "./app.css";
import "./covers.css";
import App from "./App.svelte";

// WebKit only applies :active (the press feedback) on touch once a touch listener exists.
document.addEventListener("touchstart", () => {}, { passive: true });

const target = document.getElementById("app");
if (!target) throw new Error("missing #app");

export default mount(App, { target });
