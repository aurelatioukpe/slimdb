# SlimDB — Prompt directeur de réalisation

> Document de référence unique : positionnement, scope, architecture, garde-fous.
> Sert à la fois de spec technique et de prompt projet si utilisé avec un agent IA
> (par exemple collé comme fichier d'instructions dans un outil comme Claude Code).
>
> v1 — synthèse des échanges de conception, durcie sur les points de risque identifiés
> (timeline MVP, séquencement des tests, compatibilité de format, prudence sur les benchmarks).

## 0. Règle d'arbitrage

En cas de conflit entre « ajouter quelque chose qui semble utile » et « respecter le scope
documenté ici » : le scope documenté gagne toujours. Toute idée hors scope va dans un
backlog séparé, jamais directement dans le code.

## 1. Mission et positionnement

**Mission.** Construire une base de données document, embarquée, persistante,
zéro-configuration, pour applications locales.

**Proposition.**

> Remplace tes fichiers config.json / save.json / cache par une vraie base de données —
> sans jamais faire tourner de serveur.

**Ce que SlimDB n'est pas**, sauf décision explicite de changement de version majeure :

- pas une base serveur ou réseau
- pas un concurrent de PostgreSQL ou MongoDB à grande échelle
- pas une base analytique
- pas un système distribué
- pas un moteur pensé pour des millions de documents

**Cible**, par ordre de priorité : Godot/gamedev → apps desktop → outils CLI → IoT/edge →
Python/Node en écosystème élargi.

**Terrain déjà occupé** (à connaître, pas à copier) : UnQLite, RedDB, BisonDB, FerrumDB
couvrent déjà « embedded + document + persistant ». Le différenciateur de SlimDB n'est pas
une fonctionnalité isolée — c'est la combinaison d'une expérience minimale
(Open → Set → Done), de bindings natifs prêts à l'emploi, et de garanties de fiabilité
démontrées publiquement plutôt qu'annoncées.

## 2. Principes de conception non négociables

- **Open → Set → Done.** Aucune configuration, aucun serveur, aucune migration obligatoire.
- **Safe par défaut.** Mode de durabilité par défaut : safe (fsync sur transaction). Le
  développeur ne doit jamais avoir besoin de comprendre fsync() pour obtenir une base fiable.
- **Slim veut dire slim.** Pas de daemon, pas de process secondaire, dépendances minimales.
- **Toujours récupérable.** Un fichier .sdb existant ne devient jamais illisible suite à un
  crash, un kill -9, ou une coupure de courant.
- **Limites documentées plutôt que promesses vagues.** Annoncer explicitement le domaine de
  validité (< 100 000 documents, < 1 Go, single process) et s'y tenir.
- **Pas de fonctionnalité disque sans test de crash.** Toute modification touchant à
  l'écriture disque repasse par la boucle de crash test avant d'être considérée terminée.

## 3. Modèle de données

Types : null, boolean, integer, float, string, binary, array, object.

Accès par chemin :

```
db.get("player.level")
db.set("player.level", 43)
db.delete("player.inventory")
db.exists("player.name")
```

Collections comme simples namespaces logiques (pas de notion relationnelle) :

```
players = db.collection("players")
players.set("aurel", { "name": "Aurel", "level": 42 })
```

## 4. Surface d'API v1 — et rien de plus

Core : open, close, get, set, delete, exists, has, keys, update, clear, transaction,
flush, stats, compact, backup, restore.

Requêtes (volontairement limitées, pas de pipeline d'agrégation) : find() avec
$eq $ne $gt $gte $lt $lte $in $exists, éventuellement $and $or.

Index : create_index(path, unique=bool), fonctionnel sur des chemins imbriqués
(profile.country).

Objectif : l'ensemble de l'API doit s'apprendre en 10 minutes.

## 5. Architecture technique

- Langage du core : Rust — sécurité mémoire, FFI C, PyO3, N-API, GDExtension, cross-platform.
- Le C ABI est le contrat fondamental. Tous les bindings reposent sur
  slimdb_open/close/get/set/delete/begin/commit/rollback, jamais sur les structures internes du core.
- Séparation mémoire (HashMap actif) / stockage (WAL + fichier .sdb) : chaque écriture met à
  jour la mémoire, ajoute au WAL, puis fsync selon le mode de durabilité.

Arborescence indicative :

```
slimdb/
├── core/        (engine, document, storage, wal, transaction, index, recovery, compaction)
├── bindings/    (c, python, node, godot)
├── cli/
├── benchmarks/
├── tests/
└── docs/
```

## 6. Format de fichier & garanties de fiabilité

- Extension .sdb, format binaire versionné (pas JSON — bon modèle d'API, mauvais format de
  stockage durable).
- Header : magic, format_version, database_id, checksum.
- Chaque record : MAGIC | VERSION | TYPE | LENGTH | SEQUENCE | PAYLOAD | CHECKSUM.
- Écriture partielle détectée au redémarrage → troncature propre, jamais de base illisible.
- Compaction et backup : toujours fichier temporaire + fsync + rename atomique.
  Jamais delete puis create.
- Format documenté publiquement dès la Phase 1.

**Ajout obligatoire — test de compatibilité ascendante.** Avant chaque release mineure, un
fichier .sdb généré par la version précédente doit s'ouvrir sans erreur ni perte avec la
nouvelle version. C'est le test le plus souvent oublié dans ce genre de projet, et celui qui
casse la confiance des premiers utilisateurs le plus vite si on le saute.

## 7. Modes de durabilité

| Mode      | Comportement            | Usage                                              |
|-----------|-------------------------|----------------------------------------------------|
| safe (défaut) | fsync à chaque transaction | comportement par défaut, aucune configuration requise |
| balanced  | flush périodique        | usage explicite, documenté comme compromis         |
| fast      | RAM + WAL bufferisé     | usage explicite, pour cas non critiques            |

Point d'attention à ne pas sauter : le coût réel de fsync sur cibles contraintes
(export mobile / Switch depuis Godot) peut être significativement plus élevé que sur desktop.
Mesurer et documenter les chiffres par plateforme avant de publier un objectif de perf
générique — ne jamais recopier un chiffre d'un autre projet.

## 8. Scope v0.1 exact (MVP, ordonné par dépendance réelle)

**Bloc 1 — Moteur** (bloque tout le reste, ne pas paralléliser avec le Bloc 3)

- Rust core, fichier unique .sdb, valeurs JSON-like
- get / set / delete / exists par chemin
- persistance automatique via WAL
- checksum/framing par record
- crash recovery (troncature propre)
- écritures atomiques (fichier temporaire + rename)
- boucle de crash test automatisée en CI — condition de sortie du Bloc 1, pas une case à
  cocher parmi d'autres

**Bloc 2 — Fiabilité applicative**

- transactions (begin/commit/rollback, atomicité, rollback sur échec)
- compaction

**Bloc 3 — Utilisabilité** (démarre seulement après que le Bloc 1 passe le crash test de façon stable)

- CLI (open get set keys stats compact)
- binding Python (premier binding — preuve que l'architecture FFI fonctionne)

Explicitement hors du MVP v0.1 : collections avancées, index, requêtes, Node, Godot,
encryption, TTL. Ces éléments arrivent en v0.2+ (cf. roadmap).

## 9. Hors scope explicite (jusqu'à décision explicite de version majeure)

Jamais en v1 : réplication, réseau, clustering, SQL, API compatible MongoDB, pipeline
d'agrégation, sharding, transactions distribuées, dashboard web, encryption obligatoire,
cloud, ORM.

## 10. Stratégie de tests — la vraie colonne vertébrale du projet

- **Unitaires** : document, paths, sérialisation, WAL, index, transactions.
- **Intégration** : write→close→reopen ; crash→recovery ; compaction→reopen ;
  backup→restore ; version ancienne→nouvelle (compatibilité de format).
- **Property tests** : séquences aléatoires de set/delete/update/transaction/crash/reopen,
  vérifiant état_après_recovery == état_attendu.
- **Fuzzing** : fichiers de base, records WAL, documents JSON, requêtes.
- **Test de crash à grande échelle, en boucle, en CI** (pas seulement en local avant une release) :

```
pour i en 1..1_000_000 :
    opération aléatoire
    kill aléatoire du process
    réouverture
    vérifier les invariants
```

C'est ce test, plus que n'importe quelle fonctionnalité, qui distingue SlimDB d'un wrapper
HashMap. Il doit tourner avant tout travail de binding.

## 11. Roadmap réaliste

Les estimations sont des ordres de grandeur pour un rythme side-project (quelques
soirs/week-ends par semaine), pas des engagements fermes — à recalibrer après la Phase 0
selon l'aisance réelle avec Rust.

| Phase | Contenu | Estimation | Condition de passage à la suite |
|---|---|---|---|
| 0 — Prototype jetable | HashMap + JSON + get/set/delete + fichier basique, sans WAL | quelques jours à 1 semaine | valide les choix d'API, rien à conserver |
| 1 — Moteur réel | format binaire, WAL, recovery, checksum, écritures atomiques, crash test en CI | 4 à 8 semaines | le test de crash à 1M itérations passe de façon stable |
| 2 — API stable | transactions, compaction, stats, backup/restore, CLI | 2 à 4 semaines | CLI utilisable pour débugger une vraie base |
| 3 — Binding Python | PyO3, wheels multi-OS, packaging pip | 2 à 3 semaines | pip install slimdb sans compilation locale |
| 4 — Node | N-API sur C ABI déjà stable | 1 à 2 semaines | npm install slimdb fonctionnel |
| 5 — Godot (killer feature) | GDExtension, intégration user://, jeu de démo | 2 à 3 semaines | démo survit à un kill -9 en écriture |
| 6 — Durcissement | fuzzing étendu, benchmarks mesurés, CI cross-platform, compat. de format formalisée | continu | avant tout tag v1.0 |

Total réaliste avant une v1.0 sérieuse : plusieurs mois, pas quelques semaines. Ce n'est pas
une raison de repousser le début — c'est une raison de publier tôt (dès la fin de la Phase 1
ou 3) pour du feedback réel, plutôt que d'attendre d'avoir « tout » avant de montrer le projet.

## 12. Definition of Done — jalons clés

- **Phase 1 terminée ⟺** le test de crash à 1 000 000 itérations passe en CI sans divergence
  d'état, aucune corruption observée sur kill -9 aléatoire, format de fichier documenté
  (même en interne, dans /docs).
- **Phase 3 terminée ⟺** pip install slimdb fonctionne sans compilation locale sur
  Linux x86_64, macOS ARM64/x86_64, Windows x86_64.
- **Phase 5 terminée ⟺** un jeu Godot minimal, tué en pleine écriture, rouvre sans perte
  au-delà de la dernière transaction commitée.

## 13. Garde-fous — à ne jamais enfreindre

- Aucune fonctionnalité hors scope (section 9) sans mise à jour explicite de ce document.
- Aucune modification du format de fichier ou du WAL sans repasser la boucle de crash test.
- Aucun chiffre de performance publié sans avoir été mesuré sur la plateforme cible réelle.
- Aucune rupture silencieuse de compatibilité de format entre deux versions mineures —
  migration explicite ou refus explicite avec message clair, jamais un échec silencieux.
- Aucun binding développé avant que le moteur ne passe le test de crash de façon stable.
- En cas de doute entre « simple maintenant » et « extensible pour plus tard » : choisir
  simple. Le format versionné permet d'étendre sans sur-ingénierie immédiate.

## 14. Conventions de code & documentation

- Result<T, SlimDbError> partout dans le core ; aucun unwrap() ou panic!() hors tests.
- Aucun unsafe sans commentaire justificatif et test dédié.
- Le C ABI (slimdb_*) est le seul point de contact stable entre le core et les bindings.
- Chaque module public documenté avec exemples testables (cargo test --doc).
- README compréhensible en 30 secondes :

```
# SlimDB
The tiny embedded database for local apps.
No server. No configuration. No SQL.
```

## 15. Barre à franchir avant toute communication publique

Le démonstrateur Godot doit, sans script caché ni retouche manuelle :

- écrire une sauvegarde (player.level, player.inventory, etc.)
- être tué brutalement en pleine écriture suivante
- être relancé et retrouver exactement la dernière donnée commitée, sans corruption

Si cette démo n'est pas fiable à 100 %, aucune communication publique (post, README, vidéo)
ne doit affirmer que SlimDB est crash-safe.
