import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// GitHub Pages serves the product site at /swarm/ and these docs at /swarm/docs/.
export default defineConfig({
  site: 'https://driesh.github.io',
  base: '/swarm/docs',
  integrations: [
    starlight({
      title: 'Swarm docs',
      logo: { src: './src/assets/bee.svg', alt: 'Swarm' },
      favicon: '/favicon.png',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/DriesH/swarm' }],
      editLink: { baseUrl: 'https://github.com/DriesH/swarm/edit/main/docs-site/' },
      customCss: [
        '@fontsource/geist-sans/400.css',
        '@fontsource/geist-sans/500.css',
        '@fontsource/geist-sans/600.css',
        '@fontsource/geist-mono/400.css',
        './src/styles/theme.css',
      ],
      sidebar: [
        { label: 'Start here', items: ['install', 'first-project'] },
        { label: 'Using Swarm', items: ['how-work-starts', 'workspace', 'swarm-view', 'review', 'pull-requests', 'notifications-and-usage'] },
        { label: 'Configure', items: ['settings', 'project-config', 'keyboard'] },
        { label: 'Reference', items: ['harness', 'troubleshooting'] },
      ],
    }),
  ],
});
