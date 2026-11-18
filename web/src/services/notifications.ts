import { reactive, readonly } from 'vue'

export type AppNotificationStatus = 'information' | 'success' | 'error' | 'neutral'

export interface AppNotificationInput {
  status?: AppNotificationStatus
  title: string
  description?: string
  /** 毫秒；0 表示常驻，需手动关闭 */
  duration?: number
  /** 同一 key 正在展示时不会重复入栈 */
  key?: string
}

export interface AppNotification extends AppNotificationInput {
  id: number
  status: AppNotificationStatus
  description: string
  duration: number
  timestamp: string
}

const MAX_ITEMS = 4
let sequence = 0

const state = reactive<{ items: AppNotification[] }>({ items: [] })

export const notifications = readonly(state)

function defaultDuration(status: AppNotificationStatus): number {
  if (status === 'error') return 8000
  if (status === 'success') return 4500
  if (status === 'information') return 6000
  return 5000
}

export function notify(input: AppNotificationInput): number {
  const status = input.status ?? 'information'
  const title = input.title.trim()
  if (!title) return -1

  if (input.key && state.items.some((item) => item.key === input.key)) {
    return -1
  }

  while (state.items.length >= MAX_ITEMS) {
    state.items.shift()
  }

  const id = ++sequence
  state.items.push({
    id,
    key: input.key,
    status,
    title,
    description: input.description?.trim() ?? '',
    duration: input.duration ?? defaultDuration(status),
    timestamp: new Date().toLocaleTimeString('zh-CN', { hour12: false }),
  })
  return id
}

export function dismissNotification(id: number) {
  const index = state.items.findIndex((item) => item.id === id)
  if (index >= 0) state.items.splice(index, 1)
}

export function clearNotifications() {
  state.items.splice(0)
}

export function notifySuccess(title: string, description = '', duration?: number) {
  return notify({ status: 'success', title, description, duration })
}

export function notifyError(title: string, description = '', duration?: number) {
  return notify({ status: 'error', title, description, duration })
}

export function notifyInfo(title: string, description = '', duration?: number) {
  return notify({ status: 'information', title, description, duration })
}

export function notifyWarning(title: string, description = '', duration = 0) {
  return notify({ status: 'information', title, description, duration })
}
