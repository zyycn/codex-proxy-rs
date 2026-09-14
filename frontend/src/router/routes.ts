import type { RouteRecordRaw } from 'vue-router'

export const routes: RouteRecordRaw[] = [
  {
    path: '/login',
    name: 'login',
    component: () => import('@/views/login/index.vue'),
  },
  {
    path: '/key',
    component: () => import('@/layout/AppLayout.vue'),
    meta: { role: 'key' },
    redirect: { name: 'key-overview' },
    children: [
      {
        path: 'overview',
        name: 'key-overview',
        component: () => import('@/views/overview/index.vue'),
      },
      {
        path: 'usage',
        name: 'key-usage',
        component: () => import('@/views/usage/index.vue'),
      },
      {
        path: 'theme',
        name: 'key-theme',
        component: () => import('@/views/theme/index.vue'),
      },
    ],
  },
  {
    path: '/',
    component: () => import('@/layout/AppLayout.vue'),
    meta: { role: 'admin' },
    children: [
      {
        path: '',
        name: 'dashboard',
        component: () => import('@/views/overview/index.vue'),
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
        path: 'account-groups',
        name: 'account-groups',
        component: () => import('@/views/groups/index.vue'),
      },
      {
        path: 'api-keys',
        name: 'api-keys',
        component: () => import('@/views/keys/index.vue'),
      },
      {
        path: 'usage',
        name: 'usage',
        component: () => import('@/views/usage/index.vue'),
      },
      {
        path: 'theme',
        name: 'theme',
        component: () => import('@/views/theme/index.vue'),
      },
      {
        path: 'settings',
        name: 'settings',
        component: () => import('@/views/settings/index.vue'),
      },
      {
        path: 'settings/backup',
        name: 'settings-backup',
        component: () => import('@/views/settings/index.vue'),
      },
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    redirect: '/',
  },
]
