---
title: The Field guide
description: Catch bees and wasps that visit the title bar while agents work, and read about them in the Field guide.
---

While your agents work, a pixel bee or wasp now and then flies through the title bar. Click it to catch it. Each creature you catch goes into the Field guide.

There are 20 creatures. Each one looks like a real species and has its English name and its Latin name.

## When a creature comes

A creature comes only when all of this is true:

- at least one agent is **Working**
- **Settings › Appearance › Easter eggs** is on
- the Swarm window is in front

When all of this is true, Swarm picks a random wait of 1 to 30 minutes. The wait counts down only while all of this stays true. When one of the conditions stops, for example when you switch to another app, the wait pauses. When all are true again, the wait continues from where it stopped. It does not start again.

At most 4 creatures come in 24 hours, and two creatures come at least 45 minutes apart.

A creature stays for 45 seconds, then leaves. It flies along the empty part of the title bar, from right to left. It never covers a tab, a button or terminal text, and it never takes the keyboard focus. The velvet ant has no wings, so it crawls.

With **Reduce motion** on in macOS, the creature does not move. It sits in the title bar for 45 seconds.

## Catch it

- Click the creature.
- Or press ⌘K and choose **Catch the** and the creature's name. This row is there only while the creature is.

A toast at the bottom right says `<name> caught`. It adds **New** the first time you catch that creature, and how often you caught it after that. A catch never replaces a **Ready for review** toast. It shows after that toast has gone.

VoiceOver says `<name> appeared` once when a creature comes. This needs **Easter eggs** on, like everything else here.

## Rarity

Each creature has a tier. The tier sets how often it comes.

| Tier | Creatures | Chance |
|---|---|---|
| Common | 10 | 70 % |
| Rare | 6 | 25 % |
| Ultra rare | 4 | 5 % |

## Open the Field guide

Open it with **Field guide** in the sidebar, with ⌘K, or from the rail. The sidebar row shows how many you caught, for example `3/20`.

The guide shows the 20 creatures in three groups, one for each tier.

- A creature you caught shows its animated sprite, its names, its tier, how often you caught it and a **Wikipedia** link.
- A creature you did not catch yet shows a dark silhouette, a `?` and its tier.

Use the arrow keys to move between the cards. Home and End go to the first and the last card. On a creature you caught, ↩ opens its Wikipedia page. Esc closes the guide.

With **Reduce motion** on in macOS, the sprites show a still frame.

## Where it is kept

The daemon keeps what you caught in its database. It stays when you close the app or restart Swarm. It is not in `settings.toml`.

The daemon also keeps when each creature came for the last 24 hours. The limit of 4 creatures and the gap of 45 minutes hold after a restart too.

If you catch a creature while the app has no connection to the daemon, the catch shows at once and goes to the daemon when the connection is back.

On a screen that is not Retina, the creature in the title bar and in the toast is drawn from every second pixel of its sprite, so no part of it drops out.

## The creatures

| Tier | English name | Latin name |
|---|---|---|
| Common | Western honey bee | *Apis mellifera* |
| Common | Honey bee drone | *Apis mellifera* (male) |
| Common | Buff-tailed bumblebee | *Bombus terrestris* |
| Common | Violet carpenter bee | *Xylocopa violacea* |
| Common | Red mason bee | *Osmia bicornis* |
| Common | Patchwork leafcutter bee | *Megachile centuncularis* |
| Common | Orange-legged furrow bee | *Halictus rubicundus* |
| Common | Tawny mining bee | *Andrena fulva* |
| Common | Common wasp | *Vespula vulgaris* |
| Common | European paper wasp | *Polistes dominula* |
| Rare | Blue-banded bee | *Amegilla cingulata* |
| Rare | Green orchid bee | *Euglossa dilemma* |
| Rare | Ruby-tailed wasp | *Chrysis ignita* |
| Rare | Black-and-yellow mud dauber | *Sceliphron caementarium* |
| Rare | European hornet | *Vespa crabro* |
| Rare | Large velvet ant | *Mutilla europaea* |
| Ultra rare | Queen bee | *Apis mellifera* (queen) |
| Ultra rare | Asian giant hornet | *Vespa mandarinia* |
| Ultra rare | Tarantula hawk | *Pepsis grossa* |
| Ultra rare | Wallace's giant bee | *Megachile pluto* |
