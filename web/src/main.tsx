import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import App from './App.tsx'
import { I18nProvider } from './i18n/I18nProvider.tsx'
import { applyLocale, initialLocale, load } from './i18n/locale.ts'
import './index.css'

// Only the chosen language's catalog is fetched, before the first render.
void load(initialLocale()).then((initial) => {
  applyLocale(initial)
  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <I18nProvider initial={initial}>
        <App />
      </I18nProvider>
    </StrictMode>,
  )
})
