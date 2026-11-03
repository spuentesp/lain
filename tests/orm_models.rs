use lain::graph::GraphDatabase;
use lain::schema::{EdgeType, GraphNode, NodeType, RepoNamespace};
use lain::server::sensors::orm_model_sensor::{
    analyze_orm_models, scan_workspace_orm_models, OrmAnalysis,
};
use std::path::Path;

fn write(root: &Path, path: &str, content: &str) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn relation(analysis: &OrmAnalysis, edge: EdgeType, from: &str, to: &str) -> bool {
    analysis
        .relations
        .iter()
        .any(|relation| relation.edge_type == edge && relation.from == from && relation.to == to)
}

#[test]
fn django_model_table_column_and_migration_are_linked() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "shop/models.py",
        "from django.db import models\nclass Product(models.Model):\n    name = models.CharField(db_column='display_name')\n    class Meta:\n        db_table = 'products'\n",
    );
    write(
        tmp.path(),
        "shop/migrations/0001_product.py",
        "from django.db import migrations\nclass Migration(migrations.Migration):\n    operations = [\n        migrations.AddField(model_name='product', name='name', field=models.CharField(db_column='display_name'))\n    ]\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(&analysis, EdgeType::MapsTo, "Product", "products"));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "products",
        "display_name"
    ));
    assert!(analysis
        .columns
        .iter()
        .any(|column| { column.name == "display_name" && column.migration.is_some() }));
}

#[test]
fn sqlalchemy_model_table_column_and_migration_are_linked() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "models.py",
        "from sqlalchemy import Column, String\nclass Product(Base):\n    __tablename__ = 'products'\n    name = Column('display_name', String)\n",
    );
    write(
        tmp.path(),
        "alembic/versions/0001.py",
        "def upgrade():\n    op.add_column('products', sa.Column('display_name', sa.String()))\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(&analysis, EdgeType::MapsTo, "Product", "products"));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "products",
        "display_name"
    ));
    assert!(analysis
        .columns
        .iter()
        .any(|column| column.migration.is_some()));
}

#[test]
fn prisma_model_table_column_and_migration_are_linked() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "schema.prisma",
        "model Product {\n  id Int @id @default(autoincrement())\n  name String @map(\"display_name\")\n  @@map(\"products\")\n}\n",
    );
    write(
        tmp.path(),
        "migrations/0001_init/migration.sql",
        "CREATE TABLE products (id INTEGER PRIMARY KEY);\nALTER TABLE products ADD COLUMN display_name TEXT;\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(&analysis, EdgeType::MapsTo, "Product", "products"));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "products",
        "display_name"
    ));
    assert!(analysis
        .columns
        .iter()
        .any(|column| column.migration.is_some()));
}

#[test]
fn drizzle_model_table_column_and_migration_are_linked() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/schema.ts",
        "import { pgTable, serial, varchar } from 'drizzle-orm/pg-core';\nexport const products = pgTable('products', {\n  id: serial('id'),\n  name: varchar('display_name'),\n});\n",
    );
    write(
        tmp.path(),
        "migrations/0001.sql",
        "ALTER TABLE products ADD COLUMN display_name varchar(255);\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(
        &analysis,
        EdgeType::MapsTo,
        "products",
        "products"
    ));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "products",
        "display_name"
    ));
    assert!(analysis
        .columns
        .iter()
        .any(|column| column.migration.is_some()));
}

#[test]
fn typeorm_model_table_column_and_migration_are_linked() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/Product.ts",
        "@Entity({ name: 'products' })\nexport class Product {\n  @Column({ name: 'display_name' })\n  name: string;\n}\n",
    );
    write(
        tmp.path(),
        "migrations/0001.ts",
        "await queryRunner.query('ALTER TABLE products ADD COLUMN display_name varchar');\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(&analysis, EdgeType::MapsTo, "Product", "products"));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "products",
        "display_name"
    ));
    assert!(analysis
        .columns
        .iter()
        .any(|column| column.migration.is_some()));
}

#[test]
fn sequelize_model_table_column_and_migration_are_linked() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/Product.js",
        "const Product = sequelize.define('Product', {\n  name: { type: DataTypes.STRING, column: 'display_name' },\n}, { tableName: 'products' });\n",
    );
    write(
        tmp.path(),
        "migrations/0001.js",
        "await queryInterface.query('ALTER TABLE products ADD COLUMN display_name varchar');\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(&analysis, EdgeType::MapsTo, "Product", "products"));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "products",
        "display_name"
    ));
    assert!(analysis
        .columns
        .iter()
        .any(|column| column.migration.is_some()));
}

#[test]
fn dynamic_names_stay_unresolved_and_graph_contains_first_class_relations() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/Product.js",
        "const Product = sequelize.define('Product', { name: { type: DataTypes.STRING } }, { tableName: process.env.TABLE });\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(!analysis
        .relations
        .iter()
        .any(|relation| relation.to == "process.env.TABLE"));
    assert!(!analysis.unresolved.is_empty());

    write(
        tmp.path(),
        "schema.prisma",
        "model Product {\n  id Int @id\n  name String @map(\"display_name\")\n  @@map(\"products\")\n}\n",
    );
    let graph = GraphDatabase::new(&tmp.path().join("graph.bin")).unwrap();
    scan_workspace_orm_models(&graph, tmp.path(), &RepoNamespace::for_test()).unwrap();
    assert!(graph
        .get_nodes_by_type(NodeType::Column)
        .unwrap()
        .iter()
        .any(|node| node.name == "display_name"));
    assert!(graph
        .all_edges()
        .iter()
        .any(|edge| edge.edge_type == EdgeType::MapsTo));
    assert!(graph
        .all_edges()
        .iter()
        .any(|edge| edge.edge_type == EdgeType::HasColumn));
}

#[test]
fn multiple_classes_do_not_share_columns_and_existing_class_nodes_are_reused() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "shop/models.py",
        "from django.db import models\nclass First(models.Model):\n    first_col = models.CharField()\n    class Meta:\n        db_table = 'first'\nclass Second(models.Model):\n    second_col = models.CharField()\n    class Meta:\n        db_table = 'second'\n",
    );
    write(
        tmp.path(),
        "src/entities.ts",
        "@Entity({ name: 'first' })\nexport class First {\n  @Column()\n  first_col: string;\n}\n@Entity({ name: 'second' })\nexport class Second {\n  @Column()\n  second_col: string;\n}\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "first",
        "first_col"
    ));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "second",
        "second_col"
    ));
    assert!(!relation(
        &analysis,
        EdgeType::HasColumn,
        "first",
        "second_col"
    ));
    assert!(!relation(
        &analysis,
        EdgeType::HasColumn,
        "second",
        "first_col"
    ));

    write(
        tmp.path(),
        "shop/product.py",
        "from django.db import models\nclass Product(models.Model):\n    class Meta:\n        db_table = 'products'\n",
    );
    let graph = GraphDatabase::new(&tmp.path().join("graph.bin")).unwrap();
    let namespace = RepoNamespace::for_test();
    let mut existing = GraphNode::new_in(
        NodeType::Class,
        "Product".into(),
        "shop/product.py".into(),
        &namespace,
    );
    existing.id = GraphNode::generate_id(
        &NodeType::Class,
        "shop/product.py",
        "Product",
        Some(0),
        &namespace,
    );
    existing.line_start = Some(0);
    existing.line_end = Some(3);
    graph.insert_nodes_batch(&[existing.clone()]).unwrap();
    scan_workspace_orm_models(&graph, tmp.path(), &namespace).unwrap();
    let products: Vec<_> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|node| node.node_type == NodeType::Class && node.name == "Product")
        .collect();
    assert_eq!(products.len(), 1, "{products:#?}");
    assert_eq!(products[0].id, existing.id);
}

#[test]
fn dynamic_column_expressions_stay_unresolved() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "models.py",
        "from django.db import models\nclass Product(models.Model):\n    name = models.CharField(db_column=dynamic_name)\n    class Meta:\n        db_table = 'products'\nclass Other(Base):\n    __tablename__ = 'other'\n    name = Column(dynamic_name, String)\n",
    );
    write(
        tmp.path(),
        "src/Product.ts",
        "@Entity({ name: 'products' })\nexport class Product {\n  @Column({ name: process.env.COLUMN })\n  name: string;\n}\n",
    );
    write(
        tmp.path(),
        "src/Product.js",
        "const Product = sequelize.define('Product', { name: { type: DataTypes.STRING, column: process.env.COLUMN } }, { tableName: 'products' });\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(!analysis.columns.iter().any(|column| column.name == "name"));
    assert!(!analysis
        .relations
        .iter()
        .any(|relation| relation.to == "dynamic_name" || relation.to == "process.env.COLUMN"));
    assert!(!analysis.unresolved.is_empty());
}

#[test]
fn prisma_and_sequelize_defaults_and_model_init_are_extracted() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "schema.prisma",
        "model OrderItem {\n  id Int @id\n  name String\n}\n",
    );
    write(
        tmp.path(),
        "src/OrderItem.js",
        "const OrderItem = sequelize.define('OrderItem', { name: { type: DataTypes.STRING } }, {});\nclass Product extends Model {}\nProduct.init({ name: { type: DataTypes.STRING, column: 'display_name' } }, { sequelize, tableName: 'products' });\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(
        &analysis,
        EdgeType::MapsTo,
        "OrderItem",
        "OrderItem"
    ));
    assert!(relation(
        &analysis,
        EdgeType::MapsTo,
        "OrderItem",
        "OrderItems"
    ));
    assert!(relation(&analysis, EdgeType::MapsTo, "Product", "products"));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "products",
        "display_name"
    ));
}

#[test]
fn rescan_preserves_orm_nodes_edges_and_well_formedness() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "schema.prisma",
        "model Product {\n  id Int @id\n  name String @map(\"display_name\")\n  @@map(\"products\")\n}\n",
    );
    let graph = GraphDatabase::new(&tmp.path().join("graph.bin")).unwrap();
    let first = scan_workspace_orm_models(&graph, tmp.path(), &RepoNamespace::for_test()).unwrap();
    let first_nodes = graph.node_count();
    let first_edges = graph.all_edges().len();
    let second = scan_workspace_orm_models(&graph, tmp.path(), &RepoNamespace::for_test()).unwrap();
    assert_eq!(second, first, "{:#?}", graph.get_all_nodes());
    assert_eq!(graph.node_count(), first_nodes);
    assert_eq!(graph.all_edges().len(), first_edges);
    graph.assert_well_formed();
}

#[test]
fn nested_argument_parentheses_keep_literal_column_names() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "models.py",
        "from django.db import models\nclass Product(models.Model):\n    name = models.CharField(max_length=10, default=make_default(), db_column='display_name')\n    class Meta:\n        db_table = 'products'\nclass Price(Base):\n    __tablename__ = 'prices'\n    amount = Column(String(50), name='price_usd')\n",
    );
    write(
        tmp.path(),
        "src/Product.ts",
        "@Entity({ name: 'products' })\nexport class Product {\n  @Column({ name: 'display_name', transformer: { to: value => value } })\n  name: string;\n}\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "products",
        "display_name"
    ));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "prices",
        "price_usd"
    ));
}

#[test]
fn migration_matching_prisma_comments_and_table_identity_are_precise() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "schema.prisma",
        "// the display label\nmodel Product {\n  id Int @id\n  name String\n  @@map(\"products\")\n}\n",
    );
    write(
        tmp.path(),
        "src/other.ts",
        "export const products = pgTable('products', { id: serial('id') });\n",
    );
    write(
        tmp.path(),
        "shop/migrations/0001_other.py",
        "from django.db import migrations\nclass Migration(migrations.Migration):\n    operations = [migrations.AddField(model_name='order', name='id', field=models.CharField(db_column='id'))]\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(!analysis.columns.iter().any(|column| column.name == "//"));
    assert!(analysis
        .columns
        .iter()
        .filter(|column| column.migration.is_some())
        .all(|column| column.table == "products"));
    let graph = GraphDatabase::new(&tmp.path().join("graph.bin")).unwrap();
    scan_workspace_orm_models(&graph, tmp.path(), &RepoNamespace::for_test()).unwrap();
    let tables: Vec<_> = graph
        .get_nodes_by_type(NodeType::Table)
        .unwrap()
        .into_iter()
        .filter(|node| node.name == "products")
        .collect();
    assert_eq!(tables.len(), 2, "{tables:#?}");
    assert!(graph
        .all_edges()
        .iter()
        .filter(|edge| edge.edge_type == EdgeType::MapsTo)
        .all(|edge| tables.iter().any(|table| table.id == edge.target_id)));
}

#[test]
fn django_defaults_and_dynamic_names_are_ast_resolved_without_guessing() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "shop/models.py",
        "from django.db import models\nclass Product(models.Model):\n    name = models.CharField(max_length=10, default=make_default(), db_column='display_name')\nclass Dynamic(models.Model):\n    class Meta:\n        db_table = TABLE_NAME\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(
        &analysis,
        EdgeType::MapsTo,
        "Product",
        "shop_product"
    ));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "shop_product",
        "display_name"
    ));
    assert!(!analysis.models.iter().any(|model| model.name == "Dynamic"));
    assert!(analysis
        .unresolved
        .iter()
        .any(|item| item.contains("Dynamic")));
}

#[test]
fn django_ast_query_honors_repository_overrides() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "shop/models.py",
        "from django.db import models\nclass Product(models.Model):\n    name = models.CharField()\n",
    );
    write(
        tmp.path(),
        ".lain/patterns/python/django-orm-model.scm",
        "(class_definition name: (identifier) @model superclasses: (argument_list) @bases body: (block) @body (#eq? @model \"__NoSuchModel__\")) @class",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(analysis.models.is_empty());
}

#[test]
fn sqlalchemy_and_python_migrations_require_structural_identity() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "models.py",
        "from sqlalchemy import Column, String\nclass Price(Base):\n    __tablename__ = 'prices'\n    amount = Column(String(50), name='price_usd')\n",
    );
    write(
        tmp.path(),
        "alembic/versions/0001.py",
        "def upgrade():\n    # prices is mentioned only in this comment.\n    op.add_column('orders', sa.Column('price_usd', sa.String()))\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "prices",
        "price_usd"
    ));
    assert!(analysis
        .columns
        .iter()
        .all(|column| column.migration.is_none()));

    write(
        tmp.path(),
        "alembic/versions/0002.py",
        "def upgrade():\n    op.add_column('prices', sa.Column('price_usd', sa.String()))\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(analysis
        .columns
        .iter()
        .any(|column| column.migration.is_some()));
}

#[test]
fn prisma_parser_ignores_comments_and_nested_attribute_parentheses() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "schema.prisma",
        "// model Fake {\n// ignored String\nmodel OrderItem {\n  id Int @id @default(autoincrement())\n  label String @default(db.generated(\"x)y\")) @map(\"display_name\")\n  // fake String @map(\"fake\")\n}\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(
        &analysis,
        EdgeType::MapsTo,
        "OrderItem",
        "OrderItem"
    ));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "OrderItem",
        "display_name"
    ));
    assert!(!analysis
        .columns
        .iter()
        .any(|column| column.name == "fake" || column.name == "//"));
}

#[test]
fn typeorm_and_drizzle_use_ast_names_and_locations() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/entities.ts",
        "@Entity()\nexport class OrderItem {\n  @Column({ name: 'display_name', transformer: { to: value => value } })\n  name: string;\n}\n",
    );
    write(
        tmp.path(),
        "src/schema.ts",
        "export const orders = pgTable('orders', {\n  total: numeric('total_amount', { precision: 10, scale: 2 }),\n});\n",
    );
    let analysis = analyze_orm_models(tmp.path());
    assert!(relation(
        &analysis,
        EdgeType::MapsTo,
        "OrderItem",
        "order_item"
    ));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "order_item",
        "display_name"
    ));
    assert!(relation(
        &analysis,
        EdgeType::HasColumn,
        "orders",
        "total_amount"
    ));
    assert!(analysis
        .columns
        .iter()
        .any(|column| column.name == "total_amount" && column.line == 2));
}
