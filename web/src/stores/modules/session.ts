import { defineStore } from 'pinia'

export interface SessionUser {
  name: string
  role: string
}

export const useSessionStore = defineStore('session', () => {
  const user: SessionUser = {
    name: 'admin',
    role: '管理员',
  }

  return {
    user,
  }
})
