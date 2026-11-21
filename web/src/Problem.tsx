import { useI18n } from './i18n/context'

/** A failure in words: the core's own sentence when it refused, as it sent it. Nothing for null. */
export function Problem({ error }: { error: unknown }) {
  const { errorText } = useI18n()
  if (error === null) return null
  return <p className="error" role="alert">{errorText(error)}</p>
}
