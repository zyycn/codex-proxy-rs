import type { RouteRecordRaw } from 'vue-router'
import type { AuthSession } from '@/api'

declare module 'vue-router' {
  interface RouteMeta {
    role?: AuthSession['role']
    defaultEntry?: boolean
    guestOnly?: boolean
  }
}

export const routes: RouteRecordRaw[] = [
  {
    path: '/login',
    name: 'login',
    meta: { guestOnly: true },
    component: () => import('@/views/login/index.vue'),
  },
  {
    path: '/key-usage',
    name: 'key-usage',
    meta: { role: 'key', defaultEntry: true },
    component: () => import('@/views/key-usage/index.vue'),
  },
  {
    path: '/',
    meta: { role: 'admin', defaultEntry: true },
    component: () => import('@/layout/index.vue'),
    children: [
      {
        path: '',
        name: 'dashboard',
        component: () => import('@/views/dashboard/index.vue'),
      },
      {
        path: 'accounts',
        name: 'accounts',
        component: () => import('@/views/accounts/index.vue'),
      },
      {
        path: 'proxies',
        name: 'proxies',
        component: () => import('@/views/proxies/index.vue'),
      },
      {
        path: 'groups',
        name: 'groups',
        component: () => import('@/views/groups/index.vue'),
      },
      {
        path: 'keys',
        name: 'keys',
        component: () => import('@/views/keys/index.vue'),
      },
      {
        path: 'usage',
        name: 'usage',
        component: () => import('@/views/usage/index.vue'),
      },
      {
        path: 'plugins',
        children: [
          {
            path: '',
            name: 'plugins',
            component: () => import('@/views/plugins/index.vue'),
          },
          {
            path: ':instanceId/:pageId',
            name: 'plugin-page',
            component: () => import('@/views/plugins/page/index.vue'),
          },
        ],
      },
      {
        path: 'theme',
        name: 'theme',
        component: () => import('@/views/theme/index.vue'),
      },
      {
        path: 'settings',
        children: [
          {
            path: '',
            name: 'settings',
            component: () => import('@/views/settings/index.vue'),
          },
          {
            path: 'upstream',
            name: 'settings-upstream',
            component: () => import('@/views/settings/index.vue'),
          },
          {
            path: 'backup',
            name: 'settings-backup',
            component: () => import('@/views/settings/index.vue'),
          },
          {
            path: 'pricing',
            name: 'settings-pricing',
            component: () => import('@/views/settings/index.vue'),
          },
        ],
      },
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    redirect: '/',
  },
]
