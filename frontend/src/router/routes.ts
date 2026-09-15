import type { RouteRecordRaw } from 'vue-router'

export const routes: RouteRecordRaw[] = [
  {
    path: '/login',
    name: 'login',
    component: () => import('@/views/login/index.vue'),
  },
  {
    path: '/',
    component: () => import('@/layout/AppLayout.vue'),
    children: [
      {
        path: '',
        name: 'dashboard',
        component: () => import('@/views/overview/index.vue'),
      },
      {
        path: 'accounts',
        name: 'accounts',
        meta: { role: 'admin' },
        component: () => import('@/views/accounts/index.vue'),
      },
      {
        path: 'proxies',
        name: 'proxies',
        meta: { role: 'admin' },
        component: () => import('@/views/proxies/index.vue'),
      },
      {
        path: 'account-groups',
        name: 'account-groups',
        meta: { role: 'admin' },
        component: () => import('@/views/groups/index.vue'),
      },
      {
        path: 'api-keys',
        name: 'api-keys',
        meta: { role: 'admin' },
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
        meta: { role: 'admin' },
        component: () => import('@/views/settings/index.vue'),
      },
      {
        path: 'settings/backup',
        name: 'settings-backup',
        meta: { role: 'admin' },
        component: () => import('@/views/settings/index.vue'),
      },
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    redirect: '/',
  },
]
