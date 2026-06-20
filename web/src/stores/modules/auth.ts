import { defineStore } from 'pinia'
import { ref } from 'vue'

import { getDiagnostics, login as apiLogin, logout as apiLogout } from '@/api'
import type { LoginRequest } from '@/api'

export const useAuthStore = defineStore('auth', () => {
  const isAuthenticated = ref(false)
  const email = ref<string | undefined>()
  const accountId = ref<string | undefined>()
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function checkAuth() {
    try {
      await getDiagnostics()
      isAuthenticated.value = true
      return true
    } catch (err: any) {
      isAuthenticated.value = false
      return false
    }
  }

  async function login(payload: LoginRequest) {
    try {
      loading.value = true
      error.value = null
      await apiLogin(payload)

      isAuthenticated.value = true
      email.value = payload.username

      return true
    } catch (err: any) {
      error.value = err.message || '登录失败'
      isAuthenticated.value = false
      return false
    } finally {
      loading.value = false
    }
  }

  async function logout() {
    try {
      await apiLogout()
    } catch (err) {
      // 忽略登出错误
    } finally {
      isAuthenticated.value = false
      email.value = undefined
      accountId.value = undefined

      // 手动清除 cookie（临时方案，应该由后端清除）
      document.cookie = 'cpr_admin_session=; path=/; expires=Thu, 01 Jan 1970 00:00:00 GMT; SameSite=Lax'
    }
  }

  return {
    isAuthenticated,
    email,
    accountId,
    loading,
    error,
    checkAuth,
    login,
    logout,
  }
})
