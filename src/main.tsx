import React from "react"
import { createRoot } from "react-dom/client"
import App from "./App"
import "./styles/global.css"
import "./styles/license-restore.css"
import "./styles/account-menu.css"
import "./styles/info-privacy.css"
import "./styles/scroll-layout.css"
import "./styles/profile-license.css"
import "./styles/updater.css"

createRoot(document.getElementById("root")!).render(<React.StrictMode><App /></React.StrictMode>)
