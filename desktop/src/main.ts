import App from "./App.svelte";
import "./styles.css";
import { mount } from "svelte";

if (window.location.pathname === "/orb") {
  document.documentElement.classList.add("orb-surface");
  document.body.classList.add("orb-surface");
}

const app = mount(App, {
  target: document.getElementById("app")!,
});

export default app;
