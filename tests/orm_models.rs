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
