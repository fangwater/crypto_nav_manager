import {
  Activity,
  ArrowLeft,
  CheckCircle2,
  ChevronDown,
  CircleAlert,
  KeyRound,
  RefreshCw,
  Trash2,
  UserPlus,
  Users,
} from 'lucide-react'
import { type FormEvent, useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import {
  createUser,
  deleteUser,
  getAdminUsers,
  getStrategies,
  setUserStrategies,
  updateUser,
} from '../api'
import type { AuthSession, ManagedUser, NavRole, Strategy } from '../types'

const createdFormatter = new Intl.DateTimeFormat(undefined, {
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
  hour12: false,
})

function roleLabel(role: NavRole) {
  return role === 'admin' ? '管理员' : '普通用户'
}

function errorMessage(reason: unknown) {
  return reason instanceof Error ? reason.message : String(reason)
}

export function AdminUsersPage({ session }: { session: AuthSession }) {
  const [users, setUsers] = useState<ManagedUser[]>([])
  const [strategies, setStrategies] = useState<Strategy[]>([])
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [expandedId, setExpandedId] = useState<number | null>(null)

  const [newUsername, setNewUsername] = useState('')
  const [newPassword, setNewPassword] = useState('')
  const [newRole, setNewRole] = useState<NavRole>('user')
  const [creating, setCreating] = useState(false)
  const [createError, setCreateError] = useState<string | null>(null)

  const [draftGrants, setDraftGrants] = useState<Record<number, string[]>>({})
  const [grantBusy, setGrantBusy] = useState(false)
  const [panelError, setPanelError] = useState<string | null>(null)
  const [passwordDraft, setPasswordDraft] = useState('')
  const [panelNotice, setPanelNotice] = useState<string | null>(null)

  function load(refresh = false) {
    const controller = new AbortController()
    if (refresh) setRefreshing(true)
    else setLoading(true)
    setError(null)
    Promise.all([getAdminUsers(controller.signal), getStrategies()])
      .then(([userList, strategyList]) => {
        setUsers(userList)
        setStrategies(strategyList)
      })
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') {
          return
        }
        setError(errorMessage(reason))
      })
      .finally(() => {
        setLoading(false)
        setRefreshing(false)
      })
    return controller
  }

  useEffect(() => {
    const controller = load()
    return () => controller.abort()
  }, [])

  function toggleExpanded(user: ManagedUser) {
    setPanelError(null)
    setPanelNotice(null)
    setPasswordDraft('')
    if (expandedId === user.userId) {
      setExpandedId(null)
      return
    }
    setExpandedId(user.userId)
    setDraftGrants((current) => ({
      ...current,
      [user.userId]: user.strategySlugs,
    }))
  }

  async function submitCreate(event: FormEvent) {
    event.preventDefault()
    if (creating) return
    setCreating(true)
    setCreateError(null)
    try {
      await createUser({
        username: newUsername.trim(),
        password: newPassword,
        role: newRole,
      })
      setNewUsername('')
      setNewPassword('')
      setNewRole('user')
      load(true)
    } catch (reason: unknown) {
      setCreateError(errorMessage(reason))
    } finally {
      setCreating(false)
    }
  }

  async function saveGrants(user: ManagedUser) {
    if (grantBusy) return
    setGrantBusy(true)
    setPanelError(null)
    setPanelNotice(null)
    try {
      await setUserStrategies(user.userId, draftGrants[user.userId] ?? [])
      setPanelNotice('可见策略已更新')
      load(true)
    } catch (reason: unknown) {
      setPanelError(errorMessage(reason))
    } finally {
      setGrantBusy(false)
    }
  }

  async function resetPassword(user: ManagedUser) {
    if (grantBusy || !passwordDraft) return
    setGrantBusy(true)
    setPanelError(null)
    setPanelNotice(null)
    try {
      await updateUser(user.userId, { password: passwordDraft })
      setPasswordDraft('')
      setPanelNotice('密码已重置，该用户其他会话已注销')
    } catch (reason: unknown) {
      setPanelError(errorMessage(reason))
    } finally {
      setGrantBusy(false)
    }
  }

  async function changeRole(user: ManagedUser, role: NavRole) {
    if (grantBusy || role === user.role) return
    setGrantBusy(true)
    setPanelError(null)
    setPanelNotice(null)
    try {
      await updateUser(user.userId, { role })
      setPanelNotice('角色已更新')
      load(true)
    } catch (reason: unknown) {
      setPanelError(errorMessage(reason))
    } finally {
      setGrantBusy(false)
    }
  }

  async function removeUser(user: ManagedUser) {
    if (grantBusy) return
    if (!window.confirm(`确定删除用户「${user.username}」吗？`)) return
    setGrantBusy(true)
    setPanelError(null)
    setPanelNotice(null)
    try {
      await deleteUser(user.userId)
      setExpandedId(null)
      load(true)
    } catch (reason: unknown) {
      setPanelError(errorMessage(reason))
    } finally {
      setGrantBusy(false)
    }
  }

  return (
    <>
      <header className="app-header">
        <div className="app-header__inner">
          <Link className="brand brand--link" to="/">
            <span className="brand__mark" aria-hidden="true">
              <Activity size={19} strokeWidth={2} />
            </span>
            <div>
              <h1>Crypto NAV</h1>
              <p>用户与权限</p>
            </div>
          </Link>
          <Link className="header-nav-link" to="/">
            <ArrowLeft size={16} />
            盘子总览
          </Link>
        </div>
      </header>

      <main className="page-shell admin-page">
        <section className="admin-panel">
          <div className="section-heading">
            <div>
              <p className="eyebrow">ACCESS CONTROL</p>
              <h2>用户管理</h2>
            </div>
            <button
              className="refresh-button"
              type="button"
              onClick={() => load(true)}
              disabled={refreshing}
            >
              <RefreshCw
                size={15}
                className={refreshing ? 'is-spinning' : ''}
              />
              刷新
            </button>
          </div>

          <form className="admin-create" onSubmit={submitCreate}>
            <label className="admin-field">
              <span>用户名</span>
              <input
                type="text"
                autoComplete="off"
                placeholder="小写字母、数字、_ . -"
                value={newUsername}
                onChange={(event) => setNewUsername(event.target.value)}
              />
            </label>
            <label className="admin-field">
              <span>初始密码</span>
              <input
                type="password"
                autoComplete="new-password"
                placeholder="至少 8 位"
                value={newPassword}
                onChange={(event) => setNewPassword(event.target.value)}
              />
            </label>
            <label className="admin-field">
              <span>角色</span>
              <select
                value={newRole}
                onChange={(event) => setNewRole(event.target.value as NavRole)}
              >
                <option value="user">普通用户</option>
                <option value="admin">管理员</option>
              </select>
            </label>
            <button
              className="admin-button"
              type="submit"
              disabled={
                creating || !newUsername.trim() || newPassword.length < 8
              }
            >
              <UserPlus size={15} />
              {creating ? '创建中…' : '创建用户'}
            </button>
          </form>
          {createError && (
            <div className="auth-error" role="alert">
              <CircleAlert size={15} />
              <span>{createError}</span>
            </div>
          )}
        </section>

        {error && (
          <div className="error-state">
            <CircleAlert size={19} />
            <div>
              <strong>用户列表加载失败</strong>
              <span>{error}</span>
            </div>
          </div>
        )}

        {!loading && !error && (
          <section className="admin-panel">
            <div className="admin-user-list">
              {users.map((user) => {
                const expanded = expandedId === user.userId
                const isSelf = user.userId === session.userId
                const grants = draftGrants[user.userId] ?? user.strategySlugs
                return (
                  <div
                    className={
                      'admin-user' + (expanded ? ' admin-user--open' : '')
                    }
                    key={user.userId}
                  >
                    <button
                      className="admin-user__row"
                      type="button"
                      onClick={() => toggleExpanded(user)}
                    >
                      <span className="admin-user__name">
                        <Users size={15} />
                        {user.username}
                        {isSelf && <em>（当前）</em>}
                      </span>
                      <span
                        className={
                          'admin-role admin-role--' + user.role
                        }
                      >
                        {roleLabel(user.role)}
                      </span>
                      <span className="admin-user__grants">
                        {user.role === 'admin'
                          ? '全部策略'
                          : `${user.strategySlugs.length} 个策略`}
                      </span>
                      <span className="admin-user__created">
                        {createdFormatter.format(user.createdAtMs)}
                      </span>
                      <ChevronDown
                        size={15}
                        className={expanded ? 'is-open' : undefined}
                      />
                    </button>

                    {expanded && (
                      <div className="admin-user__panel">
                        <div className="admin-user__section">
                          <h4>角色</h4>
                          <div className="segmented">
                            {(['user', 'admin'] as const).map((role) => (
                              <button
                                key={role}
                                type="button"
                                className={
                                  user.role === role ? 'is-active' : ''
                                }
                                disabled={grantBusy || isSelf}
                                onClick={() => changeRole(user, role)}
                              >
                                {roleLabel(role)}
                              </button>
                            ))}
                          </div>
                        </div>

                        {user.role === 'user' && (
                          <div className="admin-user__section">
                            <h4>可见策略</h4>
                            <div className="admin-grant-grid">
                              {strategies.map((strategy) => {
                                const checked = grants.includes(strategy.slug)
                                return (
                                  <label
                                    className="admin-grant"
                                    key={strategy.slug}
                                  >
                                    <input
                                      type="checkbox"
                                      checked={checked}
                                      onChange={(event) => {
                                        const next = event.target.checked
                                          ? [...grants, strategy.slug]
                                          : grants.filter(
                                              (slug) =>
                                                slug !== strategy.slug,
                                            )
                                        setDraftGrants((current) => ({
                                          ...current,
                                          [user.userId]: next,
                                        }))
                                      }}
                                    />
                                    <span>{strategy.displayName}</span>
                                    <code>{strategy.slug}</code>
                                  </label>
                                )
                              })}
                            </div>
                            <button
                              className="admin-button"
                              type="button"
                              disabled={grantBusy}
                              onClick={() => saveGrants(user)}
                            >
                              <CheckCircle2 size={15} />
                              保存可见策略
                            </button>
                          </div>
                        )}

                        <div className="admin-user__section">
                          <h4>重置密码</h4>
                          <div className="admin-inline">
                            <input
                              type="password"
                              autoComplete="new-password"
                              placeholder="新密码，至少 8 位"
                              value={passwordDraft}
                              onChange={(event) =>
                                setPasswordDraft(event.target.value)
                              }
                            />
                            <button
                              className="admin-button"
                              type="button"
                              disabled={grantBusy || passwordDraft.length < 8}
                              onClick={() => resetPassword(user)}
                            >
                              <KeyRound size={15} />
                              重置
                            </button>
                          </div>
                        </div>

                        <div className="admin-user__section admin-user__danger">
                          <button
                            className="admin-button admin-button--danger"
                            type="button"
                            disabled={grantBusy || isSelf}
                            onClick={() => removeUser(user)}
                          >
                            <Trash2 size={15} />
                            删除用户
                          </button>
                        </div>

                        {panelError && (
                          <div className="auth-error" role="alert">
                            <CircleAlert size={15} />
                            <span>{panelError}</span>
                          </div>
                        )}
                        {panelNotice && (
                          <div className="admin-notice">
                            <CheckCircle2 size={15} />
                            <span>{panelNotice}</span>
                          </div>
                        )}
                      </div>
                    )}
                  </div>
                )
              })}
            </div>
          </section>
        )}
      </main>
    </>
  )
}
