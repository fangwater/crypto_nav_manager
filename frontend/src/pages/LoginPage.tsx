import { Activity, CircleAlert, LogIn } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { login } from '../api'
import type { AuthSession } from '../types'

export function LoginPage({
  onLogin,
}: {
  onLogin: (session: AuthSession) => void
}) {
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  async function submit(event: FormEvent) {
    event.preventDefault()
    if (busy) return
    setBusy(true)
    setError(null)
    try {
      onLogin(await login(username.trim(), password))
    } catch (reason: unknown) {
      const message = reason instanceof Error ? reason.message : String(reason)
      setError(
        message === 'invalid username or password'
          ? '用户名或密码错误'
          : message,
      )
    } finally {
      setBusy(false)
    }
  }

  return (
    <main className="auth-shell">
      <form className="auth-card" onSubmit={submit}>
        <div className="brand auth-card__brand">
          <span className="brand__mark" aria-hidden="true">
            <Activity size={19} strokeWidth={2} />
          </span>
          <div>
            <h1>Crypto NAV</h1>
            <p>净值管理系统</p>
          </div>
        </div>

        <label className="auth-field">
          <span>用户名</span>
          <input
            type="text"
            autoComplete="username"
            autoFocus
            value={username}
            onChange={(event) => setUsername(event.target.value)}
          />
        </label>
        <label className="auth-field">
          <span>密码</span>
          <input
            type="password"
            autoComplete="current-password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
        </label>

        {error && (
          <div className="auth-error" role="alert">
            <CircleAlert size={15} />
            <span>{error}</span>
          </div>
        )}

        <button
          className="auth-submit"
          type="submit"
          disabled={busy || !username.trim() || !password}
        >
          <LogIn size={15} />
          {busy ? '登录中…' : '登录'}
        </button>
      </form>
    </main>
  )
}
