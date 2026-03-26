from manim import *

class CrowDrinksWater(Scene):
    def construct(self):
        # 标题
        title = Text("乌鸦喝水", font_size=72, font="STHeiti")
        self.play(Write(title))
        self.wait(1)
        self.play(FadeOut(title))
        
        # 创建瓶子
        bottle_body = Rectangle(width=1.5, height=3, color=BLUE, fill_opacity=0.3)
        bottle_neck = Rectangle(width=0.8, height=0.5, color=BLUE, fill_opacity=0.3)
        bottle_neck.next_to(bottle_body, UP, buff=0)
        
        # 创建水
        water = Rectangle(width=1.4, height=0.8, color=BLUE_D, fill_opacity=0.7)
        water.align_to(bottle_body, DOWN)
        water.shift(UP * 0.1)
        
        # 创建乌鸦
        crow_body = Ellipse(width=0.8, height=0.5, color=BLACK, fill_opacity=1)
        crow_head = Circle(radius=0.25, color=BLACK, fill_opacity=1)
        crow_head.next_to(crow_body, LEFT, buff=-0.1)
        crow_beak = Triangle(color=ORANGE, fill_opacity=1)
        crow_beak.scale(0.2)
        crow_beak.next_to(crow_head, LEFT, buff=0)
        crow_beak.rotate(-30 * DEGREES)
        
        crow = VGroup(crow_body, crow_head, crow_beak)
        crow.to_edge(LEFT)
        crow.shift(DOWN * 0.5)
        
        # 显示场景
        self.play(
            FadeIn(bottle_body),
            FadeIn(bottle_neck),
            FadeIn(water)
        )
        self.play(FadeIn(crow))
        self.wait(0.5)
        
        # 乌鸦飞向瓶子
        self.play(
            crow.animate.shift(RIGHT * 3),
            run_time=2,
            path_arc=-30 * DEGREES
        )
        self.wait(0.5)
        
        # 乌鸦尝试喝水（失败）
        self.play(crow.animate.shift(DOWN * 0.3), run_time=0.5)
        self.play(crow.animate.shift(UP * 0.3), run_time=0.5)
        
        # 显示思考（问号）
        question_mark = Text("?", font_size=48, color=YELLOW)
        question_mark.next_to(crow, UP, buff=0.2)
        self.play(FadeIn(question_mark))
        self.wait(0.5)
        self.play(FadeOut(question_mark))
        
        # 乌鸦思考（移动到旁边）
        self.play(crow.animate.shift(LEFT * 1))
        self.wait(0.5)
        
        # 创建石子
        stones = VGroup()
        for i in range(8):
            stone = Circle(radius=0.15, color=GRAY, fill_opacity=1)
            stone.shift(DOWN * 2 + LEFT * 2 + UP * (i * 0.1))
            stones.add(stone)
        
        self.play(FadeIn(stones))
        
        # 乌鸦衔石子，放入瓶中
        water_height = water.get_height()
        water_target_height = 2.5  # 目标水位高度
        
        for i, stone in enumerate(stones):
            # 乌鸦移动到石子
            self.play(
                crow.animate.next_to(stone, UP, buff=0),
                run_time=0.3
            )
            
            # 衔起石子
            stone_copy = stone.copy()
            self.play(stone_copy.animate.next_to(crow_beak, RIGHT, buff=-0.1))
            
            # 移动到瓶子
            self.play(
                crow.animate.next_to(bottle_body, LEFT, buff=0.5),
                stone_copy.animate.next_to(bottle_body, UP, buff=0),
                run_time=0.5
            )
            
            # 放入石子
            self.play(
                stone_copy.animate.move_to(bottle_body.get_center() + DOWN * (1 - i * 0.15)),
                FadeOut(stone),
                run_time=0.3
            )
            
            # 水位上升
            new_height = water_height + (water_target_height - water_height) * ((i + 1) / len(stones))
            new_water = Rectangle(
                width=1.4, 
                height=new_height, 
                color=BLUE_D, 
                fill_opacity=0.7
            )
            new_water.align_to(bottle_body, DOWN)
            new_water.shift(UP * 0.1)
            
            self.play(
                Transform(water, new_water),
                run_time=0.2
            )
            water = new_water
        
        # 乌鸦喝水
        self.wait(0.5)
        
        # 乌鸦低头喝水
        self.play(crow.animate.shift(DOWN * 0.5))
        self.wait(0.5)
        
        # 显示开心
        happy_text = Text("真好喝！", font_size=36, font="STHeiti")
        happy_text.next_to(crow, UP, buff=0.3)
        self.play(FadeIn(happy_text))
        self.wait(1)
        
        # 结束语
        self.play(
            FadeOut(happy_text),
            crow.animate.shift(UP * 0.5)
        )
        
        moral = Text(
            "遇到困难要动脑筋想办法",
            font_size=48,
            font="STHeiti"
        )
        self.play(Write(moral))
        self.wait(2)
        
        # 淡出所有元素
        self.play(
            *[FadeOut(mob) for mob in self.mobjects]
        )

# 运行命令：manim -pql crow_drinks_water.py CrowDrinksWater
